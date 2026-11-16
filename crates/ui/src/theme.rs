//! Tema: «las luces de la sala». Los neutros de la ventana van teñidos del color de acento del
//! sistema, en claro y en oscuro; no hay ningún gris puro.
//!
//! **Único sitio con colores fijos** (lo comprueba `tests/no_fixed_colors.rs`): todo lo demás pide
//! los colores a [`Palette`]. Cada plano parte de un neutro y se mezcla con el acento en su medida:
//! la ventana apenas (un 3 % en claro, un 6 % en oscuro), el riel lateral más que ninguno (es el
//! plano con color decidido: en claro, más oscuro que el contenido; en oscuro, el pasillo, más
//! oscuro todavía) y las tarjetas quedan como el plano más claro. Con el acento pizarra la ventana
//! sale casi neutra; con el rojo, cálida. Los estados son las lámparas de la sala: verde de
//! «salida» (todo bien), ámbar de pasillo (aviso), rojo (error) y el piloto rojo de «en vivo»
//! ([`Tone::Live`], solo para decir que se está compartiendo).
//!
//! El acento puede venir del sistema (portal `Settings` en Linux, registro en Windows) con
//! cualquier valor; [`Palette::new`] lo ajusta para que **todo texto cumpla el contraste AA**
//! (4,5:1) con su fondo: el color de acento como texto se aclara u oscurece hasta llegar, y el
//! fondo de los botones primarios también, con texto blanco o casi negro según cuál contraste más
//! ([`ensure_contrast`]). Los tests recorren los acentos de GNOME y una muestra de tonos.

use iced_core::Color;

/// Contraste mínimo del texto normal (WCAG 2.x, nivel AA).
pub const AA: f32 = 4.5;

/// Contraste mínimo de los elementos gráficos y del texto grande (WCAG 2.x, nivel AA).
pub const AA_LARGE: f32 = 3.0;

/// Color sRGB de 8 bits por canal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Rojo.
    pub r: u8,
    /// Verde.
    pub g: u8,
    /// Azul.
    pub b: u8,
}

impl Rgb {
    /// Color a partir de sus canales.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Lee `#rrggbb` o `rrggbb`.
    pub fn parse_hex(text: &str) -> Option<Self> {
        let hex = text.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
        Some(Self::new(channel(0)?, channel(2)?, channel(4)?))
    }

    /// `#rrggbb`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Color de `f64` en `[0, 1]` (lo que da el portal `Settings`).
    pub fn from_unit(r: f64, g: f64, b: f64) -> Self {
        let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self::new(channel(r), channel(g), channel(b))
    }

    /// Color de `iced`.
    pub fn color(self) -> Color {
        Color::from_rgb8(self.r, self.g, self.b)
    }

    /// Con alfa.
    pub fn alpha(self, alpha: f32) -> Color {
        Color::from_rgba8(self.r, self.g, self.b, alpha)
    }

    /// Luminancia relativa (WCAG).
    pub fn luminance(self) -> f32 {
        let linear = |channel: u8| {
            let c = f32::from(channel) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// Mezcla con `other` (`amount` 0 = este, 1 = el otro).
    pub fn mix(self, other: Rgb, amount: f32) -> Rgb {
        let amount = amount.clamp(0.0, 1.0);
        let channel =
            |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
        Rgb::new(
            channel(self.r, other.r),
            channel(self.g, other.g),
            channel(self.b, other.b),
        )
    }
}

/// Contraste WCAG entre dos colores (de 1 a 21).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (a.luminance(), b.luminance());
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

/// Lleva `color` hacia el blanco o el negro (lo que más contraste dé contra `against`) hasta que
/// tenga `minimum` de contraste con él; si ya lo tiene, lo deja.
pub fn ensure_contrast(color: Rgb, against: Rgb, minimum: f32) -> Rgb {
    if contrast(color, against) >= minimum {
        return color;
    }
    let toward = if against.luminance() > 0.18 {
        BLACK
    } else {
        WHITE
    };
    let mut step = 0.0;
    let mut out = color;
    while step <= 1.0 {
        out = color.mix(toward, step);
        if contrast(out, against) >= minimum {
            return out;
        }
        step += 0.02;
    }
    out
}

/// Blanco.
pub const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);

/// Negro.
pub const BLACK: Rgb = Rgb::new(0x00, 0x00, 0x00);

/// Texto oscuro sobre acentos y colores de butaca claros.
const DARK_ON_ACCENT: Rgb = Rgb::new(0x1c, 0x1d, 0x22);

/// Texto claro sobre los planos oscuros (la ventana en oscuro, lo que flota sobre el vídeo).
const LIGHT_TEXT: Rgb = Rgb::new(0xf5, 0xf5, 0xf7);

/// Neutro del que sale la ventana en oscuro (y lo que flota sobre el vídeo, en los dos modos).
const NIGHT_WINDOW: Rgb = Rgb::new(0x15, 0x15, 0x18);

/// Neutro del que sale el riel en oscuro: el pasillo, lo más oscuro de la ventana.
const NIGHT_SIDEBAR: Rgb = Rgb::new(0x0a, 0x0a, 0x0c);

/// Cuánto acento lleva la ventana en oscuro.
const NIGHT_WINDOW_TINT: f32 = 0.06;

/// Cuánto acento lleva el riel en oscuro.
const NIGHT_SIDEBAR_TINT: f32 = 0.08;

/// Claro u oscuro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// Claro.
    #[default]
    Light,
    /// Oscuro.
    Dark,
}

/// Colores de acento de GNOME 47 (los que ofrece Ajustes → Apariencia).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Accent {
    /// Azul (el de siempre).
    #[default]
    Blue,
    /// Turquesa.
    Teal,
    /// Verde.
    Green,
    /// Amarillo.
    Yellow,
    /// Naranja.
    Orange,
    /// Rojo.
    Red,
    /// Rosa.
    Pink,
    /// Morado.
    Purple,
    /// Pizarra.
    Slate,
    /// Cualquier otro (el del sistema en KDE o Windows).
    Custom(Rgb),
}

impl Accent {
    /// Los de GNOME, en su orden.
    pub const NAMED: [Accent; 9] = [
        Accent::Blue,
        Accent::Teal,
        Accent::Green,
        Accent::Yellow,
        Accent::Orange,
        Accent::Red,
        Accent::Pink,
        Accent::Purple,
        Accent::Slate,
    ];

    /// Color base (el de fondo de libadwaita).
    pub fn rgb(self) -> Rgb {
        match self {
            Accent::Blue => Rgb::new(0x35, 0x84, 0xe4),
            Accent::Teal => Rgb::new(0x21, 0x90, 0xa4),
            Accent::Green => Rgb::new(0x3a, 0x94, 0x4a),
            Accent::Yellow => Rgb::new(0xc8, 0x88, 0x00),
            Accent::Orange => Rgb::new(0xed, 0x5b, 0x00),
            Accent::Red => Rgb::new(0xe6, 0x2d, 0x42),
            Accent::Pink => Rgb::new(0xd5, 0x61, 0x99),
            Accent::Purple => Rgb::new(0x91, 0x41, 0xac),
            Accent::Slate => Rgb::new(0x6f, 0x83, 0x96),
            Accent::Custom(rgb) => rgb,
        }
    }

    /// Nombre en la interfaz.
    pub fn label(self) -> String {
        match self {
            Accent::Blue => "Azul".to_owned(),
            Accent::Teal => "Turquesa".to_owned(),
            Accent::Green => "Verde".to_owned(),
            Accent::Yellow => "Amarillo".to_owned(),
            Accent::Orange => "Naranja".to_owned(),
            Accent::Red => "Rojo".to_owned(),
            Accent::Pink => "Rosa".to_owned(),
            Accent::Purple => "Morado".to_owned(),
            Accent::Slate => "Pizarra".to_owned(),
            Accent::Custom(rgb) => format!("Personalizado ({})", rgb.to_hex()),
        }
    }

    /// Clave para guardarlo (`blue`, `teal`… o `#rrggbb`).
    pub fn key(self) -> String {
        match self {
            Accent::Custom(rgb) => rgb.to_hex(),
            named => named.label().to_lowercase(),
        }
    }

    /// Lee lo que da [`Accent::key`] (también los nombres en inglés de GNOME).
    pub fn from_key(key: &str) -> Option<Accent> {
        let key = key.trim().to_lowercase();
        let named = match key.as_str() {
            "azul" | "blue" => Accent::Blue,
            "turquesa" | "teal" => Accent::Teal,
            "verde" | "green" => Accent::Green,
            "amarillo" | "yellow" => Accent::Yellow,
            "naranja" | "orange" => Accent::Orange,
            "rojo" | "red" => Accent::Red,
            "rosa" | "pink" => Accent::Pink,
            "morado" | "purple" => Accent::Purple,
            "pizarra" | "slate" => Accent::Slate,
            other => return Rgb::parse_hex(other).map(Accent::nearest),
        };
        Some(named)
    }

    /// Acento de canales en `[0, 1]` (el del portal `Settings` de Linux).
    pub fn from_unit(r: f64, g: f64, b: f64) -> Accent {
        Accent::nearest(Rgb::from_unit(r, g, b))
    }

    /// Acento de un `DWORD` ABGR (el del registro de Windows).
    pub fn from_abgr(abgr: u32) -> Accent {
        let [r, g, b, _] = abgr.to_le_bytes();
        Accent::nearest(Rgb::new(r, g, b))
    }

    /// El de GNOME idéntico a `rgb` o, si no lo hay, uno personalizado.
    pub fn nearest(rgb: Rgb) -> Accent {
        Accent::NAMED
            .into_iter()
            .find(|accent| accent.rgb() == rgb)
            .unwrap_or(Accent::Custom(rgb))
    }
}

/// Tipo de aviso o estado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Información (acento).
    Info,
    /// Todo bien.
    Success,
    /// Aviso.
    Warning,
    /// Error.
    Error,
    /// Neutro (gris).
    Neutral,
    /// «En vivo»: el piloto rojo de lo que se está compartiendo. No es un error ni un aviso: va
    /// en un punto con su texto, nunca en un aviso en línea.
    Live,
}

impl Tone {
    /// Todos, para recorrerlos.
    pub const ALL: [Tone; 6] = [
        Tone::Info,
        Tone::Success,
        Tone::Warning,
        Tone::Error,
        Tone::Neutral,
        Tone::Live,
    ];
}

/// Colores de un tono: el sólido (punto de estado, borde), el texto sobre los fondos normales, el
/// fondo suave de los avisos y el texto sobre ese fondo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneColors {
    /// Color sólido (punto, icono, borde).
    pub solid: Rgb,
    /// Texto sobre el fondo de la ventana y de las tarjetas.
    pub text: Rgb,
    /// Fondo suave (avisos en línea, insignias).
    pub soft: Rgb,
    /// Texto sobre [`ToneColors::soft`].
    pub on_soft: Rgb,
}

/// Paleta completa de un modo con un acento.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Modo.
    pub mode: Mode,
    /// Acento pedido.
    pub accent: Accent,
    /// Fondo de la ventana.
    pub window: Rgb,
    /// Fondo del riel lateral: el plano con más acento de la ventana.
    pub sidebar: Rgb,
    /// Fondo de las tarjetas, filas y campos.
    pub card: Rgb,
    /// Tarjeta bajo el ratón.
    pub card_hover: Rgb,
    /// Fondo de los diálogos y menús.
    pub dialog: Rgb,
    /// Texto principal.
    pub text: Rgb,
    /// Texto secundario (subtítulos, ayudas).
    pub text_secondary: Rgb,
    /// Texto desactivado (también AA: se tiene que poder leer por qué está desactivado).
    pub text_disabled: Rgb,
    /// Bordes y separadores: el filete fino de las tarjetas, de poco contraste.
    pub border: Rgb,
    /// Contorno que tiene que verse por sí solo: una butaca libre, un campo bajo el ratón, los
    /// puntos de una entrada.
    pub border_strong: Rgb,
    /// Borde de los campos con el foco y anillo de foco.
    pub focus: Rgb,
    /// Fondo de los botones normales.
    pub button: Rgb,
    /// Botón normal bajo el ratón.
    pub button_hover: Rgb,
    /// Botón normal pulsado.
    pub button_pressed: Rgb,
    /// Fondo del elemento elegido del riel: un relleno suave de acento sobre
    /// [`Palette::sidebar`].
    pub selected: Rgb,
    /// Relleno suave de acento sobre las tarjetas, los diálogos y los menús (la opción bajo el
    /// ratón de un desplegable, la tarjeta elegida).
    pub highlight: Rgb,
    /// Velo de los controles planos bajo el ratón o pulsados: se pinta translúcido
    /// ([`Palette::hover_alpha`], [`Palette::pressed_alpha`]) sobre el plano que haya debajo, así
    /// se ve igual en la ventana, en una tarjeta y en el riel.
    pub state: Rgb,
    /// Opacidad de [`Palette::state`] bajo el ratón.
    pub hover_alpha: f32,
    /// Opacidad de [`Palette::state`] al pulsar.
    pub pressed_alpha: f32,
    /// Acento como fondo (botón primario, interruptor activo).
    pub accent_bg: Rgb,
    /// Acento como fondo bajo el ratón.
    pub accent_bg_hover: Rgb,
    /// Texto sobre [`Palette::accent_bg`].
    pub on_accent: Rgb,
    /// Acento como texto (enlaces, botones planos destacados, el icono del destino elegido).
    pub accent_text: Rgb,
    /// Fondo destructivo.
    pub destructive_bg: Rgb,
    /// Destructivo bajo el ratón.
    pub destructive_bg_hover: Rgb,
    /// Texto sobre [`Palette::destructive_bg`].
    pub on_destructive: Rgb,
    /// Información.
    pub info: ToneColors,
    /// Éxito: el verde de «salida».
    pub success: ToneColors,
    /// Aviso: el ámbar de pasillo.
    pub warning: ToneColors,
    /// Error.
    pub error: ToneColors,
    /// Neutro.
    pub neutral: ToneColors,
    /// «En vivo»: el piloto rojo.
    pub live: ToneColors,
    /// Fondo detrás del vídeo (bandas).
    pub video_backdrop: Rgb,
    /// Fondo de las barras y paneles que flotan sobre el vídeo (con [`Palette::overlay_alpha`]):
    /// el oscuro de la sala, igual en los dos modos.
    pub overlay: Rgb,
    /// Opacidad de [`Palette::overlay`].
    pub overlay_alpha: f32,
    /// Texto sobre el vídeo.
    pub overlay_text: Rgb,
    /// Texto secundario sobre el vídeo.
    pub overlay_text_secondary: Rgb,
    /// Velo de los diálogos modales.
    pub scrim: Rgb,
    /// Opacidad del velo.
    pub scrim_alpha: f32,
    /// Sombras (solo de lo que flota).
    pub shadow: Rgb,
    /// Esqueleto de carga.
    pub skeleton: Rgb,
}

/// Los planos oscuros de la sala con el acento `base`, que son los de la ventana en oscuro y, en
/// los dos modos, los de lo que flota sobre el vídeo.
struct Night {
    /// El acento con el que se tiñe: `base`, aclarado si no se distinguiría de un plano oscuro.
    ink: Rgb,
    /// La ventana.
    window: Rgb,
    /// El riel.
    sidebar: Rgb,
}

impl Night {
    fn new(base: Rgb) -> Self {
        let ink = ensure_contrast(base, NIGHT_WINDOW, AA_LARGE);
        Self {
            ink,
            window: NIGHT_WINDOW.mix(ink, NIGHT_WINDOW_TINT),
            sidebar: NIGHT_SIDEBAR.mix(ink, NIGHT_SIDEBAR_TINT),
        }
    }

    /// Texto secundario sobre las barras del vídeo, legible también con un fotograma blanco
    /// debajo.
    fn overlay_text_secondary(&self) -> Rgb {
        let brightest = WHITE.mix(self.window, OVERLAY_ALPHA);
        ensure_contrast(LIGHT_TEXT.mix(self.window, 0.28), brightest, AA)
    }
}

/// Opacidad de las barras y paneles sobre el vídeo.
const OVERLAY_ALPHA: f32 = 0.86;

/// Relleno suave de `ink` sobre `surface` (`amount` de 0 a 1), rebajado si hiciera falta para que
/// `text` se lea encima con AA.
fn soft_fill(surface: Rgb, ink: Rgb, amount: f32, text: Rgb) -> Rgb {
    let mut amount = amount;
    loop {
        let fill = surface.mix(ink, amount);
        if amount <= 0.0 || contrast(text, fill) >= AA {
            return fill;
        }
        amount -= 0.02;
    }
}

impl Palette {
    /// Paleta del modo `mode` con el acento `accent` (ajustado al contraste AA).
    pub fn new(mode: Mode, accent: Accent) -> Self {
        let base = accent.rgb();
        let night = Night::new(base);
        let accent_bg = button_background(base);
        match mode {
            Mode::Light => {
                // La luz que tiñe: el acento, oscurecido si no se distinguiría del blanco.
                let ink = ensure_contrast(base, WHITE, AA_LARGE);
                let tint = |neutral: Rgb, amount: f32| neutral.mix(ink, amount);
                let window = tint(Rgb::new(0xf7, 0xf7, 0xf8), 0.03);
                let sidebar = tint(Rgb::new(0xec, 0xec, 0xee), 0.10);
                let card = WHITE;
                let dialog = card;
                let text = tint(Rgb::new(0x1f, 0x21, 0x26), 0.04);
                let button = tint(Rgb::new(0xe7, 0xe7, 0xeb), 0.05);
                let selected = soft_fill(sidebar, ink, 0.16, text);
                let highlight = soft_fill(card, ink, 0.12, text);
                let (hover_alpha, pressed_alpha) = (0.07, 0.12);
                let surfaces = [window, card, sidebar, dialog];
                // Donde puede ir texto secundario, de acento o de un tono: los planos, los
                // rellenos de elegido y cualquiera de ellos con el velo de «bajo el ratón».
                let grounds = [
                    window,
                    card,
                    sidebar,
                    dialog,
                    selected,
                    highlight,
                    window.mix(text, hover_alpha),
                    card.mix(text, hover_alpha),
                    sidebar.mix(text, hover_alpha),
                ];
                let tone = |solid: Rgb, text: Rgb| {
                    let soft = solid.mix(card, 0.86);
                    ToneColors {
                        solid: ensure_contrast(solid, card, AA_LARGE),
                        text: readable_on_all(text, &grounds),
                        soft,
                        on_soft: ensure_contrast(text, soft, AA),
                    }
                };
                let secondary = text.mix(window, 0.30);
                Self {
                    mode,
                    accent,
                    window,
                    sidebar,
                    card,
                    card_hover: tint(card, 0.05),
                    dialog,
                    text,
                    text_secondary: readable_on_all(secondary, &grounds),
                    text_disabled: readable_on_all(
                        text.mix(window, 0.42),
                        &[window, card, sidebar, dialog, button],
                    ),
                    border: tint(Rgb::new(0xe0, 0xe0, 0xe5), 0.06),
                    border_strong: tint(Rgb::new(0xb0, 0xb2, 0xba), 0.10),
                    focus: readable_on_all(base, &surfaces),
                    button,
                    button_hover: tint(Rgb::new(0xda, 0xda, 0xdf), 0.06),
                    button_pressed: tint(Rgb::new(0xcd, 0xcd, 0xd3), 0.07),
                    selected,
                    highlight,
                    state: text,
                    hover_alpha,
                    pressed_alpha,
                    accent_bg: accent_bg.background,
                    accent_bg_hover: ensure_contrast(
                        accent_bg.background.mix(BLACK, 0.08),
                        accent_bg.text,
                        AA,
                    ),
                    on_accent: accent_bg.text,
                    accent_text: readable_on_all(base.mix(BLACK, 0.12), &grounds),
                    destructive_bg: Rgb::new(0xc2, 0x1f, 0x33),
                    destructive_bg_hover: Rgb::new(0xa8, 0x1a, 0x2b),
                    on_destructive: WHITE,
                    info: tone(base, base.mix(BLACK, 0.12)),
                    success: tone(Rgb::new(0x1e, 0x9e, 0x5e), Rgb::new(0x13, 0x70, 0x3f)),
                    warning: tone(Rgb::new(0xe0, 0xa0, 0x19), Rgb::new(0x8a, 0x5a, 0x00)),
                    error: tone(Rgb::new(0xd7, 0x26, 0x3d), Rgb::new(0xb3, 0x19, 0x2b)),
                    neutral: tone(tint(Rgb::new(0x7a, 0x7c, 0x84), 0.08), secondary),
                    live: tone(Rgb::new(0xf0, 0x39, 0x2b), Rgb::new(0xc2, 0x2a, 0x1c)),
                    video_backdrop: BLACK,
                    overlay: night.window,
                    overlay_alpha: OVERLAY_ALPHA,
                    overlay_text: LIGHT_TEXT,
                    overlay_text_secondary: night.overlay_text_secondary(),
                    scrim: night.sidebar,
                    scrim_alpha: 0.36,
                    shadow: night.sidebar,
                    skeleton: button,
                }
            }
            Mode::Dark => {
                let ink = night.ink;
                let window = night.window;
                let sidebar = night.sidebar;
                // Cada plano que se acerca es la ventana con un poco más de luz.
                let lit = |amount: f32| window.mix(WHITE, amount);
                let card = lit(0.06);
                let dialog = lit(0.10);
                let text = LIGHT_TEXT.mix(ink, 0.02);
                let button = lit(0.13);
                let selected = soft_fill(sidebar, ink, 0.24, text);
                let highlight = soft_fill(dialog, ink, 0.20, text);
                let (hover_alpha, pressed_alpha) = (0.08, 0.14);
                let surfaces = [window, card, sidebar, dialog];
                let grounds = [
                    window,
                    card,
                    sidebar,
                    dialog,
                    selected,
                    highlight,
                    window.mix(text, hover_alpha),
                    card.mix(text, hover_alpha),
                    sidebar.mix(text, hover_alpha),
                    dialog.mix(text, hover_alpha),
                ];
                let tone = |solid: Rgb, text: Rgb| {
                    let soft = solid.mix(card, 0.78);
                    ToneColors {
                        solid: ensure_contrast(solid, card, AA_LARGE),
                        text: readable_on_all(text, &grounds),
                        soft,
                        on_soft: ensure_contrast(text, soft, AA),
                    }
                };
                let secondary = text.mix(window, 0.28);
                Self {
                    mode,
                    accent,
                    window,
                    sidebar,
                    card,
                    card_hover: lit(0.10),
                    dialog,
                    text,
                    text_secondary: readable_on_all(secondary, &grounds),
                    text_disabled: readable_on_all(
                        text.mix(window, 0.42),
                        &[window, card, sidebar, dialog, button],
                    ),
                    border: lit(0.12),
                    border_strong: lit(0.28),
                    focus: readable_on_all(base.mix(WHITE, 0.25), &surfaces),
                    button,
                    button_hover: lit(0.17),
                    button_pressed: lit(0.21),
                    selected,
                    highlight,
                    state: text,
                    hover_alpha,
                    pressed_alpha,
                    accent_bg: accent_bg.background,
                    accent_bg_hover: ensure_contrast(
                        accent_bg.background.mix(WHITE, 0.08),
                        accent_bg.text,
                        AA,
                    ),
                    on_accent: accent_bg.text,
                    accent_text: readable_on_all(base.mix(WHITE, 0.35), &grounds),
                    destructive_bg: Rgb::new(0xc2, 0x1f, 0x33),
                    destructive_bg_hover: Rgb::new(0xd4, 0x2a, 0x3f),
                    on_destructive: WHITE,
                    info: tone(base, base.mix(WHITE, 0.35)),
                    success: tone(Rgb::new(0x2f, 0xb8, 0x72), Rgb::new(0x86, 0xe3, 0xad)),
                    warning: tone(Rgb::new(0xd0, 0x96, 0x24), Rgb::new(0xf6, 0xcf, 0x6b)),
                    error: tone(Rgb::new(0xe5, 0x48, 0x4d), Rgb::new(0xff, 0x8f, 0x8a)),
                    neutral: tone(lit(0.48), secondary),
                    live: tone(Rgb::new(0xff, 0x4d, 0x3d), Rgb::new(0xff, 0x9a, 0x8c)),
                    video_backdrop: BLACK,
                    overlay: night.window,
                    overlay_alpha: OVERLAY_ALPHA,
                    overlay_text: LIGHT_TEXT,
                    overlay_text_secondary: night.overlay_text_secondary(),
                    scrim: BLACK,
                    scrim_alpha: 0.55,
                    shadow: BLACK,
                    skeleton: button,
                }
            }
        }
    }

    /// Colores de un tono.
    pub fn tone(&self, tone: Tone) -> ToneColors {
        match tone {
            Tone::Info => self.info,
            Tone::Success => self.success,
            Tone::Warning => self.warning,
            Tone::Error => self.error,
            Tone::Neutral => self.neutral,
            Tone::Live => self.live,
        }
    }

    /// Fondo y letra de la butaca de `seed` (un color estable por nombre, el mismo en toda la
    /// aplicación y en los dos modos).
    pub fn avatar(&self, seed: &str) -> (Rgb, Rgb) {
        // FNV-1a y una mezcla final: con `× 31` y el resto entre 8, los bits bajos apenas
        // cambiaban y muchos nombres cortos salían del mismo color.
        let hash = seed.bytes().fold(0x811c_9dc5_u32, |acc, byte| {
            (acc ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
        let mixed = u64::from(hash.wrapping_mul(0x9e37_79b1));
        let base = SEATS[((mixed * SEATS.len() as u64) >> 32) as usize];
        let colors = button_background(base);
        (colors.background, colors.text)
    }
}

/// Los colores de butaca: ocho tapizados de la misma luminancia (todos llevan la letra blanca con
/// AA) y tonos repartidos, para que una fila de personas se vea como un conjunto y no como ocho
/// colores de acento sueltos.
pub const SEATS: [Rgb; 8] = [
    Rgb::new(0x3a, 0x6f, 0xd8),
    Rgb::new(0x1a, 0x7f, 0x8c),
    Rgb::new(0x2e, 0x85, 0x48),
    Rgb::new(0xa3, 0x66, 0x00),
    Rgb::new(0xc4, 0x50, 0x1a),
    Rgb::new(0xcf, 0x3a, 0x4a),
    Rgb::new(0xc0, 0x48, 0x88),
    Rgb::new(0x87, 0x50, 0xc8),
];

/// Fondo y texto de un botón con color `base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filled {
    /// Fondo.
    pub background: Rgb,
    /// Texto.
    pub text: Rgb,
}

/// Fondo con texto legible (AA): blanco u oscuro, el que más contraste dé; si ninguno llega, el
/// fondo se oscurece o aclara hasta que llegue.
pub fn button_background(base: Rgb) -> Filled {
    let (white, dark) = (contrast(base, WHITE), contrast(base, DARK_ON_ACCENT));
    let text = if white >= dark { WHITE } else { DARK_ON_ACCENT };
    Filled {
        background: ensure_contrast(base, text, AA),
        text,
    }
}

/// `color` ajustado para leerse con AA sobre todos los `backgrounds` (que tienen que ser todos
/// claros o todos oscuros: se ajusta siempre en el mismo sentido).
pub fn readable_on_all(color: Rgb, backgrounds: &[Rgb]) -> Rgb {
    backgrounds.iter().fold(color, |color, background| {
        ensure_contrast(color, *background, AA)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(Rgb::parse_hex("#3584e4"), Some(Rgb::new(0x35, 0x84, 0xe4)));
        assert_eq!(
            Rgb::parse_hex("3584E4").map(Rgb::to_hex).as_deref(),
            Some("#3584e4")
        );
        assert_eq!(Rgb::parse_hex("#35"), None);
        assert_eq!(Rgb::from_unit(1.0, 0.0, 2.0), Rgb::new(255, 0, 255));
    }

    #[test]
    fn contrast_matches_the_wcag_examples() {
        assert!((contrast(WHITE, BLACK) - 21.0).abs() < 0.01);
        assert!((contrast(WHITE, WHITE) - 1.0).abs() < 0.001);
        // #767676 sobre blanco: 4,54 (el gris más claro que pasa AA).
        assert!((contrast(Rgb::new(0x76, 0x76, 0x76), WHITE) - 4.54).abs() < 0.02);
    }

    #[test]
    fn accents_round_trip_their_keys() {
        for accent in Accent::NAMED {
            assert_eq!(Accent::from_key(&accent.key()), Some(accent));
        }
        assert_eq!(Accent::from_key("teal"), Some(Accent::Teal));
        assert_eq!(Accent::from_key("#3584e4"), Some(Accent::Blue));
        let custom = Accent::from_key("#123456").expect("personalizado");
        assert_eq!(custom, Accent::Custom(Rgb::new(0x12, 0x34, 0x56)));
        assert_eq!(Accent::from_key(&custom.key()), Some(custom));
        assert_eq!(Accent::from_key("fucsia"), None);
    }

    #[test]
    fn every_tone_has_its_colours_and_live_is_not_the_error() {
        for mode in [Mode::Light, Mode::Dark] {
            let palette = Palette::new(mode, Accent::default());
            assert_eq!(palette.tone(Tone::Live), palette.live);
            assert_eq!(palette.tone(Tone::Error), palette.error);
            assert_ne!(palette.live.solid, palette.error.solid, "{mode:?}");
            let solids: std::collections::HashSet<_> = Tone::ALL
                .iter()
                .map(|tone| palette.tone(*tone).solid)
                .collect();
            assert_eq!(solids.len(), Tone::ALL.len(), "{mode:?}: un color por tono");
        }
    }

    #[test]
    fn a_soft_fill_backs_off_until_the_text_reads() {
        let text = Rgb::new(0x20, 0x20, 0x20);
        let surface = Rgb::new(0xee, 0xee, 0xee);
        // Con una tinta clara, el relleno entero.
        let light = Rgb::new(0x80, 0xb0, 0xff);
        assert_eq!(
            soft_fill(surface, light, 0.2, text),
            surface.mix(light, 0.2)
        );
        // Con una tinta que apagaría el texto, menos relleno hasta que se lea.
        let fill = soft_fill(surface, BLACK, 0.9, text);
        assert!(contrast(text, fill) >= AA);
        assert_ne!(fill, surface.mix(BLACK, 0.9));
        // Y nunca se queda en un bucle si ni el plano basta.
        assert_eq!(soft_fill(text, BLACK, 0.5, text), text);
    }

    #[test]
    fn seat_colours_share_their_luminance_and_carry_white_initials() {
        let (darkest, lightest) = SEATS.iter().fold((1.0_f32, 0.0_f32), |(low, high), seat| {
            (low.min(seat.luminance()), high.max(seat.luminance()))
        });
        assert!(
            lightest - darkest < 0.04,
            "las butacas pesan lo mismo: {darkest:.3} a {lightest:.3}"
        );
        for seat in SEATS {
            let filled = button_background(seat);
            assert_eq!(filled.background, seat, "{}: sin ajustar", seat.to_hex());
            assert_eq!(filled.text, WHITE, "{}", seat.to_hex());
        }
        let distinct: std::collections::HashSet<_> = SEATS.iter().collect();
        assert_eq!(distinct.len(), SEATS.len());
    }
}
