//! Tema: la paleta Adwaita en claro y oscuro con el color de acento del sistema.
//!
//! **Único sitio con colores fijos** (lo comprueba `tests/no_fixed_colors.rs`): todo lo demás pide
//! los colores a [`Palette`]. La base es la de libadwaita (claro: fondo `#fafafa`, tarjeta
//! `#ffffff`, texto `#2e3436`; oscuro: fondo `#1e1e1e`, tarjeta `#2a2a2a`, texto `#ffffff`) con el
//! acento azul `#3584e4` por defecto y éxito, aviso y error de la misma paleta.
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

/// Texto oscuro sobre acentos claros (el de Adwaita con opacidad 0,8 sobre blanco).
const DARK_ON_ACCENT: Rgb = Rgb::new(0x24, 0x1f, 0x31);

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
    /// Fondo de la barra lateral.
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
    /// Bordes y separadores.
    pub border: Rgb,
    /// Borde de los campos con el foco y anillo de foco.
    pub focus: Rgb,
    /// Fondo de los botones normales.
    pub button: Rgb,
    /// Botón normal bajo el ratón.
    pub button_hover: Rgb,
    /// Botón normal pulsado.
    pub button_pressed: Rgb,
    /// Fondo del elemento seleccionado de la barra lateral.
    pub selected: Rgb,
    /// Acento como fondo (botón primario, interruptor activo).
    pub accent_bg: Rgb,
    /// Acento como fondo bajo el ratón.
    pub accent_bg_hover: Rgb,
    /// Texto sobre [`Palette::accent_bg`].
    pub on_accent: Rgb,
    /// Acento como texto (enlaces, botones planos destacados).
    pub accent_text: Rgb,
    /// Fondo destructivo.
    pub destructive_bg: Rgb,
    /// Destructivo bajo el ratón.
    pub destructive_bg_hover: Rgb,
    /// Texto sobre [`Palette::destructive_bg`].
    pub on_destructive: Rgb,
    /// Información.
    pub info: ToneColors,
    /// Éxito.
    pub success: ToneColors,
    /// Aviso.
    pub warning: ToneColors,
    /// Error.
    pub error: ToneColors,
    /// Neutro.
    pub neutral: ToneColors,
    /// Fondo detrás del vídeo (bandas).
    pub video_backdrop: Rgb,
    /// Fondo de las barras y paneles que flotan sobre el vídeo (con [`Palette::overlay_alpha`]).
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
    /// Sombras.
    pub shadow: Rgb,
    /// Esqueleto de carga.
    pub skeleton: Rgb,
}

impl Palette {
    /// Paleta del modo `mode` con el acento `accent` (ajustado al contraste AA).
    pub fn new(mode: Mode, accent: Accent) -> Self {
        let base = accent.rgb();
        match mode {
            Mode::Light => {
                let window = Rgb::new(0xfa, 0xfa, 0xfa);
                let card = WHITE;
                let text = Rgb::new(0x2e, 0x34, 0x36);
                let sidebar = Rgb::new(0xf0, 0xf0, 0xf1);
                let button = Rgb::new(0xe8, 0xe8, 0xea);
                let worst = [window, card, sidebar];
                let accent_bg = button_background(base);
                let tone = |solid: Rgb, text: Rgb| ToneColors {
                    solid: ensure_contrast(solid, card, AA_LARGE),
                    text: readable_on_all(text, &worst),
                    soft: solid.mix(card, 0.86),
                    on_soft: ensure_contrast(text, solid.mix(card, 0.86), AA),
                };
                Self {
                    mode,
                    accent,
                    window,
                    sidebar,
                    card,
                    card_hover: Rgb::new(0xf3, 0xf3, 0xf4),
                    dialog: card,
                    text,
                    text_secondary: readable_on_all(Rgb::new(0x5e, 0x62, 0x66), &worst),
                    text_disabled: readable_on_all(
                        Rgb::new(0x6a, 0x6e, 0x72),
                        &[window, card, sidebar, button],
                    ),
                    border: Rgb::new(0xd9, 0xd9, 0xdc),
                    focus: readable_on_all(base, &worst),
                    button,
                    button_hover: Rgb::new(0xdd, 0xdd, 0xe0),
                    button_pressed: Rgb::new(0xd0, 0xd0, 0xd4),
                    selected: Rgb::new(0xdf, 0xdf, 0xe3),
                    accent_bg: accent_bg.background,
                    accent_bg_hover: ensure_contrast(
                        accent_bg.background.mix(BLACK, 0.08),
                        accent_bg.text,
                        AA,
                    ),
                    on_accent: accent_bg.text,
                    accent_text: readable_on_all(base.mix(BLACK, 0.12), &worst),
                    destructive_bg: Rgb::new(0xc0, 0x1c, 0x28),
                    destructive_bg_hover: Rgb::new(0xa5, 0x1d, 0x2d),
                    on_destructive: WHITE,
                    info: tone(base, base.mix(BLACK, 0.12)),
                    success: tone(Rgb::new(0x26, 0xa2, 0x69), Rgb::new(0x1b, 0x85, 0x53)),
                    warning: tone(Rgb::new(0xe5, 0xa5, 0x0a), Rgb::new(0x9c, 0x6e, 0x03)),
                    error: tone(Rgb::new(0xe0, 0x1b, 0x24), Rgb::new(0xc0, 0x1c, 0x28)),
                    neutral: tone(Rgb::new(0x77, 0x76, 0x7b), Rgb::new(0x5e, 0x5c, 0x64)),
                    video_backdrop: BLACK,
                    overlay: Rgb::new(0x1e, 0x1e, 0x1e),
                    overlay_alpha: 0.86,
                    overlay_text: WHITE,
                    overlay_text_secondary: Rgb::new(0xc0, 0xbf, 0xbc),
                    scrim: BLACK,
                    scrim_alpha: 0.32,
                    shadow: BLACK,
                    skeleton: Rgb::new(0xe8, 0xe8, 0xea),
                }
            }
            Mode::Dark => {
                let window = Rgb::new(0x1e, 0x1e, 0x1e);
                let card = Rgb::new(0x2a, 0x2a, 0x2a);
                let text = WHITE;
                let sidebar = Rgb::new(0x26, 0x26, 0x26);
                let dialog = Rgb::new(0x2e, 0x2e, 0x2e);
                let button = Rgb::new(0x38, 0x38, 0x38);
                let worst = [window, card, sidebar, dialog];
                let accent_bg = button_background(base);
                let tone = |solid: Rgb, text: Rgb| {
                    let soft = solid.mix(card, 0.78);
                    ToneColors {
                        solid: ensure_contrast(solid, card, AA_LARGE),
                        text: readable_on_all(text, &worst),
                        soft,
                        on_soft: ensure_contrast(text, soft, AA),
                    }
                };
                Self {
                    mode,
                    accent,
                    window,
                    sidebar,
                    card,
                    card_hover: Rgb::new(0x33, 0x33, 0x33),
                    dialog,
                    text,
                    text_secondary: readable_on_all(Rgb::new(0xc0, 0xbf, 0xbc), &worst),
                    text_disabled: readable_on_all(
                        Rgb::new(0x9a, 0x99, 0x96),
                        &[window, card, sidebar, dialog, button],
                    ),
                    border: Rgb::new(0x3d, 0x3d, 0x3d),
                    focus: readable_on_all(base.mix(WHITE, 0.25), &worst),
                    button,
                    button_hover: Rgb::new(0x42, 0x42, 0x42),
                    button_pressed: Rgb::new(0x4c, 0x4c, 0x4c),
                    selected: Rgb::new(0x3a, 0x3a, 0x3a),
                    accent_bg: accent_bg.background,
                    accent_bg_hover: ensure_contrast(
                        accent_bg.background.mix(WHITE, 0.08),
                        accent_bg.text,
                        AA,
                    ),
                    on_accent: accent_bg.text,
                    accent_text: readable_on_all(base.mix(WHITE, 0.35), &worst),
                    destructive_bg: Rgb::new(0xc0, 0x1c, 0x28),
                    destructive_bg_hover: Rgb::new(0xd4, 0x22, 0x30),
                    on_destructive: WHITE,
                    info: tone(base, base.mix(WHITE, 0.35)),
                    success: tone(Rgb::new(0x26, 0xa2, 0x69), Rgb::new(0x8f, 0xf0, 0xa4)),
                    warning: tone(Rgb::new(0xcd, 0x93, 0x09), Rgb::new(0xf8, 0xe4, 0x5c)),
                    error: tone(Rgb::new(0xc0, 0x1c, 0x28), Rgb::new(0xff, 0x7b, 0x63)),
                    neutral: tone(Rgb::new(0x9a, 0x99, 0x96), Rgb::new(0xc0, 0xbf, 0xbc)),
                    video_backdrop: BLACK,
                    overlay: Rgb::new(0x1e, 0x1e, 0x1e),
                    overlay_alpha: 0.86,
                    overlay_text: WHITE,
                    overlay_text_secondary: Rgb::new(0xc0, 0xbf, 0xbc),
                    scrim: BLACK,
                    scrim_alpha: 0.5,
                    shadow: BLACK,
                    skeleton: Rgb::new(0x38, 0x38, 0x38),
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
        }
    }

    /// Fondo y letra del avatar de `seed` (un color estable por nombre).
    pub fn avatar(&self, seed: &str) -> (Rgb, Rgb) {
        const AVATARS: [Rgb; 8] = [
            Rgb::new(0x35, 0x84, 0xe4),
            Rgb::new(0x21, 0x90, 0xa4),
            Rgb::new(0x3a, 0x94, 0x4a),
            Rgb::new(0xc8, 0x88, 0x00),
            Rgb::new(0xed, 0x5b, 0x00),
            Rgb::new(0xe6, 0x2d, 0x42),
            Rgb::new(0xd5, 0x61, 0x99),
            Rgb::new(0x91, 0x41, 0xac),
        ];
        let hash = seed.bytes().fold(0u32, |acc, byte| {
            acc.wrapping_mul(31).wrapping_add(u32::from(byte))
        });
        let base = AVATARS[(hash % AVATARS.len() as u32) as usize];
        let colors = button_background(base);
        (colors.background, colors.text)
    }
}

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

/// `color` ajustado para leerse con AA sobre todos los `backgrounds`.
fn readable_on_all(color: Rgb, backgrounds: &[Rgb]) -> Rgb {
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
}
