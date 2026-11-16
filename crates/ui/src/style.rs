//! Estilos de los *widgets* de `iced` con el tema de coroom: [`Theme`] implementa sus catálogos con
//! clases propias (`ButtonClass`, `ContainerClass`…) y todos los colores salen de su
//! [`Palette`]. Ningún *widget* se queda con el aspecto por defecto de `iced`.

use iced_core::border::{self, Border, Radius};
use iced_core::widget::text as core_text;
use iced_core::{Background, Color, Shadow, Vector};
use iced_widget::overlay::menu;
use iced_widget::{
    button, checkbox, container, pick_list, progress_bar, rule, scrollable, slider, svg,
    text_input, toggler,
};

use crate::font::Fonts;
use crate::motion::Motion;
use crate::theme::{AA_LARGE, Accent, Mode, Palette, Rgb, Tone, contrast, ensure_contrast};
use crate::tokens::{radius, seat};

/// Tema de la interfaz: paleta, fuentes y movimiento.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// Colores.
    pub palette: Palette,
    /// Fuentes.
    pub fonts: Fonts,
    /// Cuánto se animan los componentes (la ventana lo pone en cada dibujo: el del sistema y
    /// Apariencia, o ninguno con la entrada tomada).
    pub motion: Motion,
    /// Transición en curso del componente que se dibuja ([`crate::animate`]); `None` fuera de
    /// ellos.
    pub blend: Option<Blend>,
    /// Cambio de tono en curso de una insignia o un punto de estado
    /// ([`crate::animate::ToneFade`]): los colores de tono se mezclan desde el anterior.
    pub fade: Option<Fade>,
}

/// Transición de estado de un botón animado ([`crate::animate::Animated`]): el estado del que viene
/// y cuánto lleva (0 a 1, ya con su curva). El catálogo mezcla el estilo de los dos.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blend {
    /// Estado anterior.
    pub from: button::Status,
    /// Avance hacia el actual.
    pub progress: f32,
}

/// Cambio de tono de lo que se dibuja ([`Theme::tone`]): el tono del que viene y cuánto lleva (0 a
/// 1, ya con su curva).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fade {
    /// Tono anterior.
    pub from: Tone,
    /// Avance hacia el actual.
    pub progress: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(Mode::Light, Accent::Blue, Fonts::default())
    }
}

impl Theme {
    /// Tema del modo y el acento dados.
    pub fn new(mode: Mode, accent: Accent, fonts: Fonts) -> Self {
        Self {
            palette: Palette::new(mode, accent),
            fonts,
            motion: Motion::FULL,
            blend: None,
            fade: None,
        }
    }

    /// Los colores de `tone`, mezclados desde los del tono anterior si cambia ([`Theme::fade`]).
    pub fn tone(&self, tone: Tone) -> crate::theme::ToneColors {
        let to = self.palette.tone(tone);
        match self.fade {
            Some(fade) if fade.from != tone && fade.progress < 1.0 => {
                let from = self.palette.tone(fade.from);
                let t = fade.progress;
                crate::theme::ToneColors {
                    solid: from.solid.mix(to.solid, t),
                    text: from.text.mix(to.text, t),
                    soft: from.soft.mix(to.soft, t),
                    on_soft: from.on_soft.mix(to.on_soft, t),
                }
            }
            _ => to,
        }
    }

    /// Modo.
    pub fn mode(&self) -> Mode {
        self.palette.mode
    }

    /// Color del texto por defecto (para el `renderer::Style` de la interfaz).
    pub fn text_color(&self) -> Color {
        self.palette.text.color()
    }

    /// Fondo de la ventana.
    pub fn background(&self) -> Color {
        self.palette.window.color()
    }

    /// El color del plano `surface` (el de las barras sobre el vídeo, sin su transparencia).
    pub fn surface(&self, surface: Surface) -> Color {
        let p = &self.palette;
        match surface {
            Surface::Window => p.window,
            Surface::Sidebar => p.sidebar,
            Surface::Card => p.card,
            Surface::Dialog => p.dialog,
            Surface::Overlay => p.overlay,
        }
        .color()
    }

    /// Sombra de algo que flota: siempre desplazada hacia abajo y difuminada.
    fn shadow(&self, blur: f32, y: f32, light: f32, dark: f32) -> Shadow {
        Shadow {
            color: self.palette.shadow.alpha(match self.palette.mode {
                Mode::Light => light,
                Mode::Dark => dark,
            }),
            offset: Vector::new(0.0, y),
            blur_radius: blur,
        }
    }
}

fn rounded(radius: f32) -> Radius {
    border::radius(radius)
}

/// La silueta de una butaca de lado `side`: el respaldo redondeado arriba y la base casi recta
/// ([`crate::tokens::seat`]).
pub fn seat_radius(side: f32) -> Radius {
    let (back, base) = seat::radii(side);
    Radius {
        top_left: back,
        top_right: back,
        bottom_right: base,
        bottom_left: base,
    }
}

/// `radius` tal como hay que dárselo a una imagen (`iced_widget::image`) para que salga con esas
/// esquinas. El sombreador de imágenes de `iced_wgpu` 0.14 lee los cuatro radios girados media
/// vuelta respecto de [`Radius`] y del de los rectángulos (a la esquina de arriba a la izquierda
/// le aplica el de abajo a la derecha, y así las cuatro), de modo que una butaca con foto salía
/// boca abajo: recta arriba y redonda abajo. Aquí se le da ya girado. Con radios iguales no
/// cambia nada. Lo vigila la prueba de píxeles `a_seat_with_a_photo_has_the_same_outline…` de
/// `tests/components.rs`: si un `iced` posterior lo corrige, fallará y esto sobra.
pub fn image_radius(radius: Radius) -> Radius {
    Radius {
        top_left: radius.bottom_right,
        top_right: radius.bottom_left,
        bottom_right: radius.top_left,
        bottom_left: radius.top_right,
    }
}

/// La silueta de lo que rodea a una butaca de lado `side` a `gap` px (el anillo de quien habla):
/// la misma forma, con cada radio crecido lo que se separa.
pub fn seat_ring_radius(side: f32, gap: f32) -> Radius {
    let inner = seat_radius(side);
    Radius {
        top_left: inner.top_left + gap,
        top_right: inner.top_right + gap,
        bottom_right: inner.bottom_right + gap,
        bottom_left: inner.bottom_left + gap,
    }
}

/// El plano sobre el que se dibuja algo que tiene que saberlo: el filete que separa la lámpara de
/// una butaca es del color de lo que hay detrás, y una butaca libre sobre el vídeo lleva el
/// contorno y el número claros.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Surface {
    /// El fondo de la ventana.
    Window,
    /// El riel lateral.
    Sidebar,
    /// Una tarjeta o una fila de lista.
    #[default]
    Card,
    /// Un diálogo o un menú.
    Dialog,
    /// Una barra o un panel sobre el vídeo.
    Overlay,
}

fn outline(color: Color, width: f32, radius: f32) -> Border {
    Border {
        color,
        width,
        radius: rounded(radius),
    }
}

/// Mezcla lineal de dos colores (`t` de 0 a 1).
pub fn mix(from: Color, to: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

fn background_color(background: Option<Background>, fallback: Color) -> Option<Color> {
    match background {
        Some(Background::Color(color)) => Some(color),
        // Los degradados no se mezclan: se quedan como están.
        Some(Background::Gradient(_)) => None,
        None => Some(Color { a: 0.0, ..fallback }),
    }
}

/// Estilo de botón a medio camino de `from` a `to`. Un fondo que aparece o desaparece se funde
/// desde su mismo color transparente.
fn mix_button(from: button::Style, to: button::Style, t: f32) -> button::Style {
    let background = match (from.background, to.background) {
        (None, None) => None,
        (a, b) => {
            let reference = background_color(b, Color::TRANSPARENT)
                .or_else(|| background_color(a, Color::TRANSPARENT))
                .unwrap_or(Color::TRANSPARENT);
            match (
                background_color(a, reference),
                background_color(b, reference),
            ) {
                (Some(a), Some(b)) => Some(Background::Color(mix(a, b, t))),
                _ => to.background,
            }
        }
    };
    let border_color = if from.border.width == 0.0 {
        Color {
            a: 0.0,
            ..to.border.color
        }
    } else {
        from.border.color
    };
    button::Style {
        background,
        text_color: mix(from.text_color, to.text_color, t),
        border: Border {
            color: mix(border_color, to.border.color, t),
            width: from.border.width + (to.border.width - from.border.width) * t,
            radius: to.border.radius,
        },
        shadow: Shadow {
            color: mix(from.shadow.color, to.shadow.color, t),
            offset: from.shadow.offset + (to.shadow.offset - from.shadow.offset) * t,
            blur_radius: from.shadow.blur_radius
                + (to.shadow.blur_radius - from.shadow.blur_radius) * t,
        },
        snap: to.snap,
    }
}

// --- Botones ------------------------------------------------------------------------------------

/// Clases de botón.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ButtonClass {
    /// Normal (el neutro teñido).
    #[default]
    Normal,
    /// Acción principal (acento).
    Primary,
    /// Plano: solo texto o icono, fondo al pasar el ratón.
    Flat,
    /// Plano con el color de acento (enlaces).
    Link,
    /// Destructivo (rojo).
    Destructive,
    /// Plano destructivo (texto rojo).
    DestructiveFlat,
    /// Destructivo suave: fondo rojo tenue y texto rojo (dejar de compartir, expulsar, olvidar
    /// un servidor); el lleno ([`ButtonClass::Destructive`]) queda para cerrar la sala y salir.
    DestructiveSoft,
    /// Entrada del riel lateral: la elegida lleva un relleno suave de acento (sin franja en el
    /// borde); las demás, el velo de los controles planos al pasar el ratón.
    Nav {
        /// Si es la página actual.
        selected: bool,
    },
    /// Tarjeta entera pulsable (las grandes de Inicio, las filas de servidores).
    Card,
    /// Botón de la barra sobre el vídeo.
    Overlay {
        /// Si la opción está activa (modo inmersivo, panel abierto).
        active: bool,
    },
    /// Muestra de color del acento.
    Swatch {
        /// Color.
        color: Rgb,
        /// Si es el elegido.
        selected: bool,
    },
    /// Segmento de un selector segmentado. El fondo del elegido no lo pinta el segmento sino el
    /// selector ([`crate::animate::Segments`], con [`ButtonClass::SegmentThumb`]), que lo desliza
    /// de uno a otro.
    Segment {
        /// Si es el elegido.
        selected: bool,
    },
    /// El fondo del segmento elegido, que se desliza bajo los segmentos.
    SegmentThumb,
    /// Tarjeta de una opción excluyente (qué compartir): borde de acento si es la elegida.
    Choice {
        /// Si es la elegida.
        selected: bool,
    },
    /// [`ButtonClass::Choice`] que se enciende al elegirla: `lit` va de 0 (sin elegir) a 1
    /// (elegida); el borde se ilumina con el acento y el fondo toma su relleno suave (la tarjeta
    /// de calidad elegida). No se eleva: las tarjetas no llevan sombra.
    ChoiceLit {
        /// Cuánto está encendida, de 0 a 1.
        lit: f32,
    },
}

impl ButtonClass {
    /// El sonido al pulsar un botón de esta clase: confirmar el principal, nada en las pestañas y
    /// la navegación (suena el cambio de pantalla) y un «toc» en los demás.
    pub fn cue(self) -> Option<crate::sound::Cue> {
        match self {
            ButtonClass::Primary => Some(crate::sound::Cue::Confirm),
            ButtonClass::Segment { .. } | ButtonClass::SegmentThumb | ButtonClass::Nav { .. } => {
                None
            }
            _ => Some(crate::sound::Cue::Press),
        }
    }

    /// Si al pasar el ratón sube 1 px con sombra: los botones con relleno. Los planos, los de la
    /// barra sobre el vídeo, las pestañas y las tarjetas cambian solo de fondo.
    pub fn lifts(self) -> bool {
        matches!(
            self,
            ButtonClass::Normal
                | ButtonClass::Primary
                | ButtonClass::Destructive
                | ButtonClass::DestructiveSoft
        )
    }
}

impl button::Catalog for Theme {
    type Class<'a> = ButtonClass;

    fn default<'a>() -> Self::Class<'a> {
        ButtonClass::Normal
    }

    fn style(&self, class: &Self::Class<'_>, status: button::Status) -> button::Style {
        let to = self.button_style(*class, status);
        match self.blend {
            Some(blend) if blend.from != status && blend.progress < 1.0 => {
                mix_button(self.button_style(*class, blend.from), to, blend.progress)
            }
            _ => to,
        }
    }
}

impl Theme {
    pub(crate) fn button_style(&self, class: ButtonClass, status: button::Status) -> button::Style {
        let mut style = self.button_surface(class, status);
        if class.lifts() && status == button::Status::Hovered {
            // Al pasar, sube con su sombra (el desplazamiento lo dibuja [`crate::animate`]); el
            // principal brilla con el acento.
            style.shadow = match class {
                ButtonClass::Primary => Shadow {
                    color: self.palette.accent_bg.alpha(match self.palette.mode {
                        Mode::Light => 0.35,
                        Mode::Dark => 0.45,
                    }),
                    offset: Vector::new(0.0, 2.0),
                    blur_radius: 10.0,
                },
                _ => self.shadow(6.0, 2.0, 0.14, 0.4),
            };
        }
        style
    }

    fn button_surface(&self, class: ButtonClass, status: button::Status) -> button::Style {
        let p = &self.palette;
        let disabled = status == button::Status::Disabled;
        let hovered = status == button::Status::Hovered;
        let pressed = status == button::Status::Pressed;
        let base = button::Style {
            background: None,
            text_color: p.text.color(),
            border: outline(Color::TRANSPARENT, 0.0, radius::CONTROL),
            shadow: Shadow::default(),
            snap: true,
        };
        let filled = |bg: Rgb, hover: Rgb, text: Rgb| button::Style {
            background: Some(Background::Color(if disabled {
                p.button.color()
            } else if pressed || hovered {
                hover.color()
            } else {
                bg.color()
            })),
            text_color: if disabled {
                p.text_disabled.color()
            } else {
                text.color()
            },
            ..base
        };
        // Los controles planos no tienen fondo propio: bajo el ratón o pulsados llevan un velo
        // translúcido, que se ve igual sobre la ventana, una tarjeta o el riel.
        let veil = (hovered || pressed).then(|| {
            Background::Color(p.state.alpha(if pressed {
                p.pressed_alpha
            } else {
                p.hover_alpha
            }))
        });
        let flat = |text: Rgb| button::Style {
            background: veil,
            text_color: if disabled {
                p.text_disabled.color()
            } else {
                text.color()
            },
            ..base
        };
        match class {
            ButtonClass::Normal => {
                let bg = if pressed {
                    p.button_pressed
                } else {
                    p.button_hover
                };
                filled(p.button, bg, p.text)
            }
            ButtonClass::Primary => filled(p.accent_bg, p.accent_bg_hover, p.on_accent),
            ButtonClass::Destructive => {
                filled(p.destructive_bg, p.destructive_bg_hover, p.on_destructive)
            }
            ButtonClass::Flat => flat(p.text),
            ButtonClass::Link => flat(p.accent_text),
            ButtonClass::DestructiveFlat => flat(p.error.text),
            ButtonClass::DestructiveSoft => {
                let soft = p.error.soft.color();
                button::Style {
                    background: Some(Background::Color(if disabled {
                        p.button.color()
                    } else if pressed {
                        mix(soft, p.error.solid.color(), 0.3)
                    } else if hovered {
                        mix(soft, p.error.solid.color(), 0.15)
                    } else {
                        soft
                    })),
                    text_color: if disabled {
                        p.text_disabled.color()
                    } else {
                        p.error.on_soft.color()
                    },
                    ..base
                }
            }
            ButtonClass::Nav { selected } => button::Style {
                background: if selected {
                    Some(Background::Color(p.selected.color()))
                } else {
                    veil
                },
                text_color: p.text.color(),
                ..base
            },
            ButtonClass::Card => button::Style {
                background: Some(Background::Color(if pressed {
                    p.highlight.color()
                } else if hovered {
                    p.card_hover.color()
                } else {
                    p.card.color()
                })),
                text_color: if disabled {
                    p.text_disabled.color()
                } else {
                    p.text.color()
                },
                border: outline(
                    if hovered || pressed {
                        p.border_strong.color()
                    } else {
                        p.border.color()
                    },
                    1.0,
                    radius::CARD,
                ),
                shadow: Shadow::default(),
                snap: true,
            },
            ButtonClass::Overlay { active } => button::Style {
                background: Some(Background::Color(if active {
                    p.accent_bg.color()
                } else if hovered || pressed {
                    p.overlay_text.alpha(0.18)
                } else {
                    Color::TRANSPARENT
                })),
                text_color: if active {
                    p.on_accent.color()
                } else {
                    p.overlay_text.color()
                },
                ..base
            },
            ButtonClass::Swatch { color, selected } => button::Style {
                background: Some(Background::Color(color.color())),
                text_color: crate::theme::button_background(color).text.color(),
                border: outline(
                    if selected {
                        p.focus.color()
                    } else if hovered {
                        p.border.color()
                    } else {
                        Color::TRANSPARENT
                    },
                    if selected { 3.0 } else { 2.0 },
                    radius::PILL,
                ),
                shadow: Shadow::default(),
                snap: true,
            },
            ButtonClass::Choice { selected } => button::Style {
                background: Some(Background::Color(if selected {
                    p.highlight.color()
                } else if hovered || pressed {
                    p.card_hover.color()
                } else {
                    p.card.color()
                })),
                text_color: p.text.color(),
                border: outline(
                    if selected {
                        p.focus.color()
                    } else if hovered || pressed {
                        p.border_strong.color()
                    } else {
                        p.border.color()
                    },
                    2.0,
                    radius::CARD,
                ),
                shadow: Shadow::default(),
                snap: true,
            },
            ButtonClass::Segment { selected } => button::Style {
                background: Some(Background::Color(if hovered && !selected {
                    p.state.alpha(p.hover_alpha)
                } else {
                    Color::TRANSPARENT
                })),
                text_color: p.text.color(),
                border: outline(Color::TRANSPARENT, 0.0, radius::CONTROL - 2.0),
                shadow: Shadow::default(),
                snap: true,
            },
            ButtonClass::SegmentThumb => button::Style {
                // El elegido no depende de ser algo más claro que el grupo: lleva el relleno
                // suave de acento y un filete de acento que se ve por sí solo.
                background: Some(Background::Color(p.highlight.color())),
                text_color: p.text.color(),
                border: outline(p.focus.color(), 1.0, radius::CONTROL - 2.0),
                shadow: Shadow::default(),
                snap: true,
            },
            ButtonClass::ChoiceLit { lit } => mix_button(
                self.button_surface(ButtonClass::Choice { selected: false }, status),
                self.button_surface(ButtonClass::Choice { selected: true }, status),
                lit.clamp(0.0, 1.0),
            ),
        }
    }
}

// --- Contenedores -------------------------------------------------------------------------------

/// Clases de contenedor.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ContainerClass {
    /// Sin fondo.
    #[default]
    Transparent,
    /// Fondo de la ventana.
    Window,
    /// Riel lateral: el plano con más acento de la ventana.
    Sidebar,
    /// Cabecera de la ventana: el tono de las tarjetas, para que no se funda con la barra de
    /// título del sistema (KDE la pinta del mismo gris que la barra lateral).
    Headerbar,
    /// Tarjeta: relleno y un filete fino, sin sombra (las sombras son de lo que flota).
    Card,
    /// Tarjeta destacada con el borde del acento (una invitación que llega).
    CardAccent,
    /// Entrada: una tarjeta con cuerpo y talón ([`crate::widget::ticket`]).
    Ticket,
    /// Celda de señalética de un carácter del código de sala ([`crate::widget::code_cells`]).
    CodeCell {
        /// Radio de la celda.
        radius: f32,
    },
    /// Diálogo modal.
    Dialog,
    /// Aviso en línea del tono.
    Banner(Tone),
    /// Insignia de estado.
    Badge(Tone),
    /// Toast.
    Toast,
    /// Velo detrás de un diálogo, con lo que lleva de su fundido de entrada (0 a 1).
    Scrim(f32),
    /// Barra o panel sobre el vídeo.
    Overlay,
    /// Fondo del vídeo.
    Video,
    /// El fondo del vídeo encima de la imagen, con lo que le queda de su fundido (1: tapa del
    /// todo, 0: no se ve): la imagen aparece desde el fondo.
    VideoCover(f32),
    /// El borde del vídeo marcado con el acento (el teclado y el ratón van a la sala), sin fondo.
    VideoFocus,
    /// Bloque de texto monoespaciado (huellas, diagnóstico).
    Code,
    /// Bloque de esqueleto de carga.
    Skeleton,
    /// Butaca ocupada: el color de la persona con sus iniciales, en la silueta de la butaca.
    Seat {
        /// Fondo.
        background: Rgb,
        /// Letra.
        text: Rgb,
        /// Lado de la butaca.
        side: f32,
    },
    /// Butaca libre: el mismo contorno sin relleno, con su número.
    SeatEmpty {
        /// Lado de la butaca.
        side: f32,
        /// Sobre qué está (sobre el vídeo, el contorno y el número van claros).
        on: Surface,
    },
    /// Anillo alrededor de una butaca (quien habla, en el tono de éxito), con su misma silueta.
    SeatRing {
        /// Tono del anillo.
        tone: Tone,
        /// Lado de la butaca que rodea.
        side: f32,
        /// Separación entre la butaca y el borde exterior del anillo.
        gap: f32,
    },
    /// Lámpara de estado de una butaca: un punto del tono con un filete del color del fondo.
    SeatLamp {
        /// Tono de la lámpara.
        tone: Tone,
        /// Sobre qué está la butaca.
        on: Surface,
    },
    /// Anillo redondo del tono, sin relleno (el latido del punto «en vivo»).
    AvatarRing(Tone),
    /// Punto de estado.
    Dot(Tone),
    /// Etiqueta de tecla de un atajo.
    Keycap,
    /// Grupo del selector segmentado.
    Segmented,
    /// Píldora sobre el vídeo (el indicador de latencia de la sala).
    OverlayPill,
    /// Grupo del selector segmentado sobre el vídeo (pestañas del panel de la sala).
    OverlaySegmented,
    /// Contador de no leídos (acento, redondo), encima de un botón.
    Counter,
    /// Insignia de estado del tono sobre el vídeo (el nivel de la conexión en la barra de la
    /// sala): el color sólido translúcido, igual en claro y en oscuro.
    OverlayBadge(Tone),
    /// Barra de pasos completados (verde) o pendientes.
    Step {
        /// Si está hecho.
        done: bool,
    },
    /// Tramo de una barra proporcional (la latencia por etapa): el acento, más suave en los
    /// tramos alternos para que se distingan.
    BarSegment {
        /// Si es uno de los alternos.
        alternate: bool,
    },
    /// Tarjeta de lo que está saliendo de este equipo (la transmisión «en vivo»): el borde pasa
    /// al rojo del piloto con `live` (1 mientras se comparte; baja a 0 con el fundido al dejar de
    /// compartir) y brilla con `glow` (1 al empezar, 0 en reposo: brilla una sola vez y queda
    /// quieta, sin sombra).
    CardLive {
        /// Cuánto rojo lleva el borde, de 0 a 1.
        live: f32,
        /// El brillo del arranque, de 0 a 1.
        glow: f32,
    },
    /// La etiqueta «en vivo» con el tiempo transmitiendo: el rojo del piloto lleno, con el brillo
    /// del arranque (`glow`, de 1 a 0).
    LiveTag {
        /// El brillo del arranque, de 0 a 1.
        glow: f32,
    },
}

impl container::Catalog for Theme {
    type Class<'a> = ContainerClass;

    fn default<'a>() -> Self::Class<'a> {
        ContainerClass::Transparent
    }

    fn style(&self, class: &Self::Class<'_>) -> container::Style {
        let p = &self.palette;
        let fill = |color: Color| Some(Background::Color(color));
        let none = container::Style::default();
        match *class {
            ContainerClass::Transparent => none,
            ContainerClass::Window => container::Style {
                background: fill(p.window.color()),
                text_color: Some(p.text.color()),
                ..none
            },
            ContainerClass::Sidebar => container::Style {
                background: fill(p.sidebar.color()),
                text_color: Some(p.text.color()),
                ..none
            },
            ContainerClass::Headerbar => container::Style {
                background: fill(p.card.color()),
                text_color: Some(p.text.color()),
                ..none
            },
            ContainerClass::Card | ContainerClass::Ticket => container::Style {
                background: fill(p.card.color()),
                text_color: Some(p.text.color()),
                border: outline(p.border.color(), 1.0, radius::CARD),
                shadow: Shadow::default(),
                snap: true,
            },
            ContainerClass::CardAccent => container::Style {
                background: fill(p.highlight.color()),
                text_color: Some(p.text.color()),
                border: outline(p.focus.color(), 1.5, radius::CARD),
                shadow: Shadow::default(),
                snap: true,
            },
            ContainerClass::CodeCell { radius } => container::Style {
                background: fill(p.highlight.color()),
                text_color: Some(p.text.color()),
                border: outline(Color::TRANSPARENT, 0.0, radius),
                shadow: Shadow::default(),
                snap: true,
            },
            ContainerClass::Dialog => container::Style {
                background: fill(p.dialog.color()),
                text_color: Some(p.text.color()),
                border: outline(p.border.color(), 1.0, radius::DIALOG),
                shadow: self.shadow(28.0, 8.0, 0.22, 0.55),
                snap: true,
            },
            ContainerClass::Banner(tone) => {
                let tone = self.tone(tone);
                container::Style {
                    background: fill(tone.soft.color()),
                    text_color: Some(tone.on_soft.color()),
                    border: outline(tone.solid.alpha(0.35), 1.0, radius::CARD),
                    ..none
                }
            }
            ContainerClass::Badge(tone) => {
                let tone = self.tone(tone);
                container::Style {
                    background: fill(tone.soft.color()),
                    text_color: Some(tone.on_soft.color()),
                    border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
                    ..none
                }
            }
            ContainerClass::Toast => container::Style {
                background: fill(p.overlay.alpha(0.96)),
                text_color: Some(p.overlay_text.color()),
                border: outline(Color::TRANSPARENT, 0.0, radius::CARD),
                shadow: self.shadow(16.0, 4.0, 0.25, 0.5),
                snap: true,
            },
            ContainerClass::Scrim(shown) => container::Style {
                background: fill(p.scrim.alpha(p.scrim_alpha * shown.clamp(0.0, 1.0))),
                ..none
            },
            ContainerClass::Overlay => container::Style {
                background: fill(p.overlay.alpha(p.overlay_alpha)),
                text_color: Some(p.overlay_text.color()),
                border: outline(p.overlay_text.alpha(0.08), 1.0, radius::CARD),
                ..none
            },
            ContainerClass::Video => container::Style {
                background: fill(p.video_backdrop.color()),
                text_color: Some(p.overlay_text.color()),
                ..none
            },
            ContainerClass::VideoCover(left) => container::Style {
                background: fill(p.video_backdrop.alpha(left.clamp(0.0, 1.0))),
                ..none
            },
            ContainerClass::VideoFocus => container::Style {
                border: outline(p.accent_bg.color(), 2.0, 0.0),
                ..none
            },
            ContainerClass::Code => container::Style {
                background: fill(p.window.color()),
                text_color: Some(p.text.color()),
                border: outline(p.border.color(), 1.0, radius::CONTROL),
                ..none
            },
            ContainerClass::Skeleton => container::Style {
                background: fill(p.skeleton.color()),
                border: outline(Color::TRANSPARENT, 0.0, radius::CONTROL),
                ..none
            },
            ContainerClass::Seat {
                background,
                text,
                side,
            } => container::Style {
                background: fill(background.color()),
                text_color: Some(text.color()),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: seat_radius(side),
                },
                ..none
            },
            ContainerClass::SeatEmpty { side, on } => {
                let (line, number) = match on {
                    Surface::Overlay => (p.overlay_text.alpha(0.42), p.overlay_text_secondary),
                    _ => (p.border_strong.color(), p.text_secondary),
                };
                container::Style {
                    text_color: Some(number.color()),
                    border: Border {
                        color: line,
                        width: seat::OUTLINE,
                        radius: seat_radius(side),
                    },
                    ..none
                }
            }
            ContainerClass::SeatRing { tone, side, gap } => container::Style {
                border: Border {
                    color: self.tone(tone).solid.color(),
                    width: 2.0,
                    radius: seat_ring_radius(side, gap),
                },
                ..none
            },
            ContainerClass::SeatLamp { tone, on } => container::Style {
                background: fill(self.tone(tone).solid.color()),
                border: outline(self.surface(on), seat::LAMP_GAP, radius::PILL),
                ..none
            },
            ContainerClass::AvatarRing(tone) => container::Style {
                border: outline(self.tone(tone).solid.color(), 2.0, radius::PILL),
                ..none
            },
            ContainerClass::Dot(tone) => container::Style {
                background: fill(self.tone(tone).solid.color()),
                border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
                ..none
            },
            ContainerClass::Keycap => container::Style {
                background: fill(p.button.color()),
                text_color: Some(p.text.color()),
                border: outline(p.border.color(), 1.0, radius::CELL),
                shadow: Shadow {
                    color: p.border.color(),
                    offset: Vector::new(0.0, 1.0),
                    blur_radius: 0.0,
                },
                snap: true,
            },
            ContainerClass::Segmented => container::Style {
                background: fill(p.button.color()),
                border: outline(Color::TRANSPARENT, 0.0, radius::CONTROL),
                ..none
            },
            ContainerClass::OverlayPill => container::Style {
                background: fill(p.overlay.alpha(p.overlay_alpha)),
                text_color: Some(p.overlay_text.color()),
                border: outline(p.overlay_text.alpha(0.08), 1.0, radius::PILL),
                ..none
            },
            ContainerClass::OverlaySegmented => container::Style {
                background: fill(p.overlay_text.alpha(0.10)),
                border: outline(Color::TRANSPARENT, 0.0, radius::CONTROL),
                ..none
            },
            ContainerClass::OverlayBadge(tone) => container::Style {
                background: fill(self.tone(tone).solid.alpha(0.22)),
                text_color: Some(p.overlay_text.color()),
                border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
                ..none
            },
            ContainerClass::Counter => container::Style {
                background: fill(p.accent_bg.color()),
                text_color: Some(p.on_accent.color()),
                border: outline(p.overlay.alpha(p.overlay_alpha), 2.0, radius::PILL),
                ..none
            },
            ContainerClass::Step { done } => container::Style {
                background: fill(if done {
                    p.success.solid.color()
                } else {
                    p.border.color()
                }),
                border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
                ..none
            },
            ContainerClass::BarSegment { alternate } => container::Style {
                background: fill(p.accent_bg.alpha(if alternate { 0.62 } else { 0.95 })),
                border: outline(Color::TRANSPARENT, 0.0, 4.0),
                ..none
            },
            ContainerClass::CardLive { live, glow } => {
                let live = live.clamp(0.0, 1.0);
                let glow = glow.clamp(0.0, 1.0);
                let red = p.live.solid.color();
                container::Style {
                    background: fill(p.card.color()),
                    text_color: Some(p.text.color()),
                    border: outline(
                        mix(p.border.color(), red, live),
                        1.0 + 0.5 * live,
                        radius::CARD,
                    ),
                    // El brillo del arranque: una luz que cae y se apaga. En reposo, nada.
                    shadow: Shadow {
                        color: p.live.solid.alpha(0.45 * glow),
                        offset: Vector::new(0.0, 2.0),
                        blur_radius: 18.0 * glow,
                    },
                    snap: true,
                }
            }
            ContainerClass::LiveTag { glow } => {
                let glow = glow.clamp(0.0, 1.0);
                // El piloto, oscurecido lo justo para llevar la letra clara con AA.
                let pilot = ensure_contrast(p.live.solid, p.on_destructive, crate::theme::AA);
                container::Style {
                    background: fill(pilot.color()),
                    text_color: Some(p.on_destructive.color()),
                    border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
                    shadow: Shadow {
                        color: p.live.solid.alpha(0.5 * glow),
                        offset: Vector::new(0.0, 2.0),
                        blur_radius: 12.0 * glow,
                    },
                    snap: true,
                }
            }
        }
    }
}

// --- Texto --------------------------------------------------------------------------------------

/// Clases de texto.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TextClass {
    /// El color del contenedor (por defecto, el del texto principal).
    #[default]
    Inherit,
    /// Principal.
    Primary,
    /// Secundario.
    Secondary,
    /// Desactivado.
    Disabled,
    /// Acento.
    Accent,
    /// Del tono (sobre fondos normales).
    Tone(Tone),
    /// Sobre el vídeo.
    Overlay,
    /// Secundario sobre el vídeo.
    OverlaySecondary,
    /// Un color de una paleta concreta (el tono o el acento de la paleta oscura sobre el vídeo,
    /// con la ventana en claro).
    Custom(Color),
}

impl core_text::Catalog for Theme {
    type Class<'a> = TextClass;

    fn default<'a>() -> Self::Class<'a> {
        TextClass::Inherit
    }

    fn style(&self, class: &Self::Class<'_>) -> core_text::Style {
        let p = &self.palette;
        let color = match *class {
            TextClass::Inherit => None,
            TextClass::Primary => Some(p.text),
            TextClass::Secondary => Some(p.text_secondary),
            TextClass::Disabled => Some(p.text_disabled),
            TextClass::Accent => Some(p.accent_text),
            TextClass::Tone(tone) => Some(self.tone(tone).text),
            TextClass::Overlay => Some(p.overlay_text),
            TextClass::OverlaySecondary => Some(p.overlay_text_secondary),
            TextClass::Custom(color) => {
                return core_text::Style { color: Some(color) };
            }
        };
        core_text::Style {
            color: color.map(Rgb::color),
        }
    }
}

// --- Campos de texto ----------------------------------------------------------------------------

/// Clases de campo de texto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputClass {
    /// Normal.
    #[default]
    Normal,
    /// Con un error de validación.
    Invalid,
}

impl text_input::Catalog for Theme {
    type Class<'a> = InputClass;

    fn default<'a>() -> Self::Class<'a> {
        InputClass::Normal
    }

    fn style(&self, class: &Self::Class<'_>, status: text_input::Status) -> text_input::Style {
        let p = &self.palette;
        // El campo no tiene relleno propio (es del color de la tarjeta): lo que dice dónde está
        // es su borde, que se ve por sí solo también en reposo. Con el foco, el anillo de acento,
        // más grueso; con un error, el del error.
        let border = match (class, status) {
            (InputClass::Invalid, _) => outline(p.error.solid.color(), 2.0, radius::CONTROL),
            (_, text_input::Status::Focused { .. }) => {
                outline(p.focus.color(), 2.0, radius::CONTROL)
            }
            (_, text_input::Status::Hovered) => {
                outline(p.text_secondary.color(), 1.0, radius::CONTROL)
            }
            _ => outline(p.outline.color(), 1.0, radius::CONTROL),
        };
        text_input::Style {
            background: Background::Color(if status == text_input::Status::Disabled {
                p.button.color()
            } else {
                p.card.color()
            }),
            border,
            icon: p.text_secondary.color(),
            // Más tenue que el valor. En un campo desactivado no se pinta: el valor ya va en el
            // color más tenue que cumple AA, así que el ejemplo se leería como algo escrito (y
            // un ejemplo de qué escribir no dice nada donde no se puede escribir).
            placeholder: if status == text_input::Status::Disabled {
                Color::TRANSPARENT
            } else {
                p.placeholder.color()
            },
            value: if status == text_input::Status::Disabled {
                p.text_disabled.color()
            } else {
                p.text.color()
            },
            selection: p.accent_bg.alpha(0.35),
        }
    }
}

// --- Interruptores y casillas -------------------------------------------------------------------

impl toggler::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> Self::Class<'a> {}

    fn style(&self, _class: &Self::Class<'_>, status: toggler::Status) -> toggler::Style {
        let p = &self.palette;
        let (on, hovered, disabled) = match status {
            toggler::Status::Active { is_toggled } => (is_toggled, false, false),
            toggler::Status::Hovered { is_toggled } => (is_toggled, true, false),
            toggler::Status::Disabled { is_toggled } => (is_toggled, false, true),
        };
        // El tirador es lo más claro del interruptor en los dos modos; encendido, si el acento
        // es tan claro que no se vería, el color del texto sobre el acento.
        let light = match p.mode {
            Mode::Light => p.card,
            Mode::Dark => p.text,
        };
        let knob = if disabled && on {
            // Sobre el relleno suave de acento, que con acentos muy claros se acerca al gris del
            // tirador.
            ensure_contrast(p.text_disabled, p.highlight, AA_LARGE)
        } else if disabled {
            p.text_disabled
        } else if on && contrast(light, p.accent_bg) < AA_LARGE {
            p.on_accent
        } else {
            light
        };
        // Apagado, un carril gris lleno; encendido, el acento. Desactivado no es «apagado»: el
        // carril se vacía y queda su contorno (con el relleno suave de acento si estaba
        // encendido), así que se ve que está ahí, en qué posición y que no responde.
        let track = match (on, hovered, disabled) {
            (true, _, true) => p.highlight.color(),
            (false, _, true) => Color::TRANSPARENT,
            (true, true, _) => p.accent_bg_hover.color(),
            (true, false, _) => p.accent_bg.color(),
            (false, true, _) => p.outline.mix(p.text, 0.18).color(),
            (false, false, _) => p.outline.color(),
        };
        toggler::Style {
            background: Background::Color(track),
            background_border_width: if disabled { 1.5 } else { 0.0 },
            background_border_color: if disabled {
                p.outline.color()
            } else {
                Color::TRANSPARENT
            },
            foreground: Background::Color(knob.color()),
            foreground_border_width: 0.0,
            foreground_border_color: Color::TRANSPARENT,
            text_color: Some(if disabled { p.text_disabled } else { p.text }.color()),
            border_radius: None,
            padding_ratio: 0.12,
        }
    }
}

impl checkbox::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> Self::Class<'a> {}

    fn style(&self, _class: &Self::Class<'_>, status: checkbox::Status) -> checkbox::Style {
        let p = &self.palette;
        let (checked, hovered, disabled) = match status {
            checkbox::Status::Active { is_checked } => (is_checked, false, false),
            checkbox::Status::Hovered { is_checked } => (is_checked, true, false),
            checkbox::Status::Disabled { is_checked } => (is_checked, false, true),
        };
        checkbox::Style {
            background: Background::Color(if checked {
                if hovered {
                    p.accent_bg_hover
                } else {
                    p.accent_bg
                }
                .color()
            } else {
                p.card.color()
            }),
            icon_color: p.on_accent.color(),
            border: outline(
                if checked {
                    p.accent_bg
                } else {
                    p.text_secondary
                }
                .color(),
                if checked { 0.0 } else { 2.0 },
                6.0,
            ),
            text_color: Some(if disabled { p.text_disabled } else { p.text }.color()),
        }
    }
}

// --- Desplazamiento, selectores y menús ---------------------------------------------------------

impl scrollable::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> Self::Class<'a> {}

    fn style(&self, _class: &Self::Class<'_>, status: scrollable::Status) -> scrollable::Style {
        let p = &self.palette;
        let (hovered, dragged) = match status {
            scrollable::Status::Active { .. } => (false, false),
            scrollable::Status::Hovered {
                is_vertical_scrollbar_hovered,
                is_horizontal_scrollbar_hovered,
                ..
            } => (
                is_vertical_scrollbar_hovered || is_horizontal_scrollbar_hovered,
                false,
            ),
            scrollable::Status::Dragged { .. } => (true, true),
        };
        let scroller = scrollable::Scroller {
            background: Background::Color(p.text_secondary.alpha(if dragged {
                0.8
            } else if hovered {
                0.6
            } else {
                0.35
            })),
            border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
        };
        let rail = scrollable::Rail {
            background: None,
            border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
            scroller,
        };
        scrollable::Style {
            container: container::Style::default(),
            vertical_rail: rail,
            horizontal_rail: rail,
            gap: None,
            auto_scroll: scrollable::AutoScroll {
                background: Background::Color(p.dialog.color()),
                border: outline(p.border.color(), 1.0, radius::PILL),
                shadow: self.shadow(8.0, 2.0, 0.15, 0.4),
                icon: p.text.color(),
            },
        }
    }
}

impl menu::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> <Self as menu::Catalog>::Class<'a> {}

    fn style(&self, _class: &<Self as menu::Catalog>::Class<'_>) -> menu::Style {
        let p = &self.palette;
        menu::Style {
            background: Background::Color(p.dialog.color()),
            border: outline(p.border.color(), 1.0, radius::CONTROL),
            text_color: p.text.color(),
            selected_text_color: p.text.color(),
            selected_background: Background::Color(p.highlight.color()),
            shadow: self.shadow(16.0, 4.0, 0.15, 0.45),
        }
    }
}

impl pick_list::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> <Self as pick_list::Catalog>::Class<'a> {}

    fn style(
        &self,
        _class: &<Self as pick_list::Catalog>::Class<'_>,
        status: pick_list::Status,
    ) -> pick_list::Style {
        let p = &self.palette;
        let background = match status {
            pick_list::Status::Active => p.button,
            pick_list::Status::Hovered | pick_list::Status::Opened { .. } => p.button_hover,
        };
        pick_list::Style {
            text_color: p.text.color(),
            placeholder_color: p.text_secondary.color(),
            handle_color: p.text_secondary.color(),
            background: Background::Color(background.color()),
            // Como los campos: un borde que se ve por sí solo en reposo (el relleno apenas se
            // aparta de la tarjeta), más marcado bajo el ratón y el anillo de acento al abrirlo.
            border: match status {
                pick_list::Status::Opened { .. } => outline(p.focus.color(), 2.0, radius::CONTROL),
                pick_list::Status::Hovered => {
                    outline(p.text_secondary.color(), 1.0, radius::CONTROL)
                }
                pick_list::Status::Active => outline(p.outline.color(), 1.0, radius::CONTROL),
            },
        }
    }
}

// --- Iconos, separadores y progreso -------------------------------------------------------------

/// Clases de icono SVG (monocromos con el color indicado).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SvgClass {
    /// El del texto.
    #[default]
    Text,
    /// Secundario.
    Secondary,
    /// Acento.
    Accent,
    /// Sobre el acento (botón primario).
    OnAccent,
    /// Sobre el rojo destructivo.
    OnDestructive,
    /// Del tono.
    Tone(Tone),
    /// Sobre el vídeo.
    Overlay,
}

impl svg::Catalog for Theme {
    type Class<'a> = SvgClass;

    fn default<'a>() -> Self::Class<'a> {
        SvgClass::Text
    }

    fn style(&self, class: &Self::Class<'_>, _status: svg::Status) -> svg::Style {
        let p = &self.palette;
        let color = match *class {
            SvgClass::Text => p.text,
            SvgClass::Secondary => p.text_secondary,
            SvgClass::Accent => p.accent_text,
            SvgClass::OnAccent => p.on_accent,
            SvgClass::OnDestructive => p.on_destructive,
            SvgClass::Tone(tone) => self.tone(tone).text,
            SvgClass::Overlay => p.overlay_text,
        };
        svg::Style {
            color: Some(color.color()),
        }
    }
}

impl rule::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> Self::Class<'a> {}

    fn style(&self, _class: &Self::Class<'_>) -> rule::Style {
        rule::Style {
            color: self.palette.border.color(),
            radius: rounded(0.0),
            fill_mode: rule::FillMode::Full,
            snap: true,
        }
    }
}

/// Clases de deslizador.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SliderClass {
    /// Sobre el fondo de la ventana o una tarjeta: raíl del acento, tirador como un botón.
    #[default]
    Accent,
    /// Sobre el panel o la barra que flotan sobre el vídeo (siempre oscuros).
    Overlay,
    /// [`SliderClass::Accent`] de algo silenciado: el raíl se apaga (el valor se conserva).
    AccentMuted,
    /// [`SliderClass::Overlay`] de algo silenciado.
    OverlayMuted,
}

impl SliderClass {
    /// La misma clase, apagada si `muted`.
    pub fn muted(self, muted: bool) -> Self {
        match (self, muted) {
            (SliderClass::Accent | SliderClass::AccentMuted, true) => SliderClass::AccentMuted,
            (SliderClass::Overlay | SliderClass::OverlayMuted, true) => SliderClass::OverlayMuted,
            (SliderClass::Accent | SliderClass::AccentMuted, false) => SliderClass::Accent,
            (SliderClass::Overlay | SliderClass::OverlayMuted, false) => SliderClass::Overlay,
        }
    }
}

impl slider::Catalog for Theme {
    type Class<'a> = SliderClass;

    fn default<'a>() -> Self::Class<'a> {
        SliderClass::Accent
    }

    fn style(&self, class: &Self::Class<'_>, status: slider::Status) -> slider::Style {
        let p = &self.palette;
        let touched = status != slider::Status::Active;
        let accent = if touched {
            p.accent_bg_hover
        } else {
            p.accent_bg
        };
        // Lo recorrido, lo que falta y el tirador. El carril vacío y el tirador se ven por sí
        // solos (3:1 con su plano): el tirador es lo que se arrastra y no puede confundirse con
        // el fondo. Silenciado, lo recorrido se apaga pero sigue distinguiéndose de lo que falta.
        let (filled, empty, handle, ring) = match *class {
            SliderClass::Accent => (accent.color(), p.outline.color(), p.focus, p.text),
            SliderClass::AccentMuted => (
                p.text_secondary.color(),
                p.outline.color(),
                p.text_secondary,
                p.text,
            ),
            SliderClass::Overlay => (
                accent.color(),
                p.overlay_text.alpha(OVERLAY_TRACK),
                p.overlay_text,
                p.focus,
            ),
            SliderClass::OverlayMuted => (
                p.overlay_text_secondary.color(),
                p.overlay_text.alpha(OVERLAY_TRACK),
                p.overlay_text,
                p.focus,
            ),
        };
        slider::Style {
            rail: slider::Rail {
                backgrounds: (Background::Color(filled), Background::Color(empty)),
                width: 4.0,
                border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 9.0 },
                background: Background::Color(handle.color()),
                // Bajo el ratón o arrastrando, un aro que lo destaca.
                border_width: if touched { 2.0 } else { 0.0 },
                border_color: if touched {
                    ring.color()
                } else {
                    Color::TRANSPARENT
                },
            },
        }
    }
}

/// Opacidad del carril vacío de un deslizador sobre el vídeo (del texto claro sobre el panel
/// oscuro): 3:1 con el panel, haya debajo un fotograma claro u oscuro.
pub const OVERLAY_TRACK: f32 = 0.45;

/// Clases de barra de progreso.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ProgressClass {
    /// Acento.
    #[default]
    Accent,
    /// Del tono.
    Tone(Tone),
}

impl progress_bar::Catalog for Theme {
    type Class<'a> = ProgressClass;

    fn default<'a>() -> Self::Class<'a> {
        ProgressClass::Accent
    }

    fn style(&self, class: &Self::Class<'_>) -> progress_bar::Style {
        let p = &self.palette;
        let bar = match *class {
            ProgressClass::Accent => p.accent_bg,
            ProgressClass::Tone(tone) => self.tone(tone).solid,
        };
        progress_bar::Style {
            background: Background::Color(p.button.color()),
            bar: Background::Color(bar.color()),
            border: outline(Color::TRANSPARENT, 0.0, radius::PILL),
        }
    }
}
