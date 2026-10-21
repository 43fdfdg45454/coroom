//! Fuentes: la del escritorio si está instalada y, si no, Inter embebida (SIL OFL 1.1).
//!
//! `iced` (con `cosmic-text`) carga las fuentes del sistema con `fontdb`, que en Linux lee la
//! configuración de `fontconfig`. [`Fonts::detect`] elige la familia de la interfaz: la que dé el
//! escritorio (por ejemplo la clave `font-name` de GNOME por el portal `Settings`), si no la primera
//! que exista de [`UI_FAMILIES`] (Adwaita Sans y Cantarell en GNOME, Inter, Noto Sans en KDE, Segoe
//! UI Variable en Windows) y, si no, la Inter embebida, que siempre se carga como respaldo. La
//! monoespaciada (códigos de sala y huellas) se elige igual de [`MONO_FAMILIES`].
//!
//! La elegida pasa a ser la familia genérica `sans-serif` (y `monospace`) de la base de fuentes,
//! así que los *widgets* usan siempre las constantes [`UI`], [`SEMIBOLD`] y [`MONO`]. Hay que
//! llamar a [`Fonts::detect`] una vez, antes de dibujar nada.

use std::borrow::Cow;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use iced_core::Font;
use iced_core::font::Weight;
use iced_widget::graphics::text::cosmic_text;

/// Inter Regular embebida (subconjunto latino, SIL OFL 1.1).
pub const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.otf");

/// Inter SemiBold embebida (subconjunto latino, SIL OFL 1.1).
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.otf");

/// Familia de la Inter embebida.
pub const EMBEDDED_FAMILY: &str = "Inter";

/// Texto normal (400).
pub const UI: Font = Font::DEFAULT;

/// Títulos, encabezados y botones (600).
pub const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..Font::DEFAULT
};

/// Códigos y huellas.
pub const MONO: Font = Font::MONOSPACE;

/// Familias de interfaz que se prueban, en orden.
pub const UI_FAMILIES: [&str; 7] = [
    "Adwaita Sans",
    "Cantarell",
    "Inter",
    "Noto Sans",
    "Segoe UI Variable Text",
    "Segoe UI",
    "Ubuntu",
];

/// Familias monoespaciadas que se prueban, en orden.
pub const MONO_FAMILIES: [&str; 8] = [
    "Adwaita Mono",
    "Source Code Pro",
    "Noto Sans Mono",
    "JetBrains Mono",
    "Cascadia Mono",
    "Consolas",
    "DejaVu Sans Mono",
    "Liberation Mono",
];

/// Fuentes elegidas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fonts {
    /// Familia de la interfaz.
    pub family: &'static str,
    /// Familia monoespaciada (`None`: la que dé la base de fuentes).
    pub mono: Option<&'static str>,
}

impl Default for Fonts {
    /// La Inter embebida (sin consultar el sistema).
    fn default() -> Self {
        Self {
            family: EMBEDDED_FAMILY,
            mono: None,
        }
    }
}

/// Nombre que pidió el escritorio (vive lo que el proceso).
static DESKTOP_FAMILY: OnceLock<String> = OnceLock::new();

impl Fonts {
    /// Carga la Inter embebida, elige las familias (ver el módulo) y las deja como genéricas.
    /// `desktop` es la fuente que pide el escritorio, si se conoce (`"Adwaita Sans 11"`).
    pub fn detect(desktop: Option<&str>) -> Self {
        let mut system = iced_widget::graphics::text::font_system()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        system.load_font(Cow::Borrowed(INTER_REGULAR));
        system.load_font(Cow::Borrowed(INTER_SEMIBOLD));
        let db = system.raw().db_mut();
        let has = |db: &cosmic_db::Database, name: &str| {
            db.faces().any(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(name))
            })
        };
        let desktop = desktop
            .map(family_of)
            .filter(|name| !name.is_empty() && has(db, name))
            .map(|name| DESKTOP_FAMILY.get_or_init(|| name.to_owned()).as_str());
        let family = desktop
            .or_else(|| UI_FAMILIES.into_iter().find(|name| has(db, name)))
            .unwrap_or(EMBEDDED_FAMILY);
        let mono = MONO_FAMILIES.into_iter().find(|name| has(db, name));
        db.set_sans_serif_family(family);
        if let Some(mono) = mono {
            db.set_monospace_family(mono);
        }
        let drop = measure_cap_drop(system.raw());
        CAP_DROP.store(drop.to_bits(), Ordering::Relaxed);
        Self { family, mono }
    }

    /// Si se usa la Inter embebida.
    pub fn is_embedded(&self) -> bool {
        self.family == EMBEDDED_FAMILY
    }
}

/// La base de fuentes de `cosmic-text`.
mod cosmic_db {
    pub use iced_widget::graphics::text::cosmic_text::fontdb::Database;
}

/// [`cap_drop`] de la letra elegida (los bits de un `f32`; 0 hasta [`Fonts::detect`]).
static CAP_DROP: AtomicU32 = AtomicU32::new(0);

/// Cuánto quedan las mayúsculas y las cifras por debajo del centro de su línea (negativo: por
/// encima), en fracción del tamaño de letra, con la letra de la interfaz en seminegrita (la de
/// las insignias). `cosmic-text` centra en el alto de la línea la caja de la letra (ascendente
/// más descendente), no las mayúsculas, así que cuánto se desvían depende de las métricas de
/// cada fuente: 0 con Inter (y Adwaita Sans), unas centésimas con otras. Se lee de la fuente al
/// elegirla ([`Fonts::detect`]); [`crate::widget::cap_centered`] lo compensa en las píldoras.
pub fn cap_drop() -> f32 {
    f32::from_bits(CAP_DROP.load(Ordering::Relaxed))
}

/// [`cap_drop`] de la familia `sans-serif` de `system` en seminegrita, con las mismas métricas
/// que usa `cosmic-text` para colocar la línea; 0 si la fuente no dice su altura de mayúsculas.
fn measure_cap_drop(system: &mut cosmic_text::FontSystem) -> f32 {
    use cosmic_text::fontdb::{Family, Query, Stretch, Style, Weight as DbWeight};
    let query = Query {
        families: &[Family::SansSerif],
        weight: DbWeight::SEMIBOLD,
        stretch: Stretch::Normal,
        style: Style::Normal,
    };
    let Some(id) = system.db().query(&query) else {
        return 0.0;
    };
    let Some(font) = system.get_font(id, DbWeight::SEMIBOLD) else {
        return 0.0;
    };
    let metrics = font.metrics();
    match metrics.cap_height {
        Some(cap) => cap_drop_of(metrics.ascent, metrics.descent, cap, metrics.units_per_em),
        None => 0.0,
    }
}

/// [`cap_drop`] de una fuente con ascendente `ascent`, descendente `descent` (negativo, como lo
/// dan las métricas) y mayúsculas de `cap`, en unidades de `units_per_em`. En una línea de alto L
/// la base cae en (L - (a + d)) / 2 + a (con d positivo), las mayúsculas van de ahí `cap` hacia
/// arriba y su centro queda (a - d - cap) / 2 por debajo de L / 2.
fn cap_drop_of(ascent: f32, descent: f32, cap: f32, units_per_em: u16) -> f32 {
    if units_per_em == 0 {
        return 0.0;
    }
    (ascent + descent - cap) / 2.0 / f32::from(units_per_em)
}

/// La familia de una descripción de fuente de GTK o KDE: `"Cantarell 11"` → `"Cantarell"`,
/// `"Noto Sans,10,-1,5,50,0,0,0,0,0"` → `"Noto Sans"`.
pub fn family_of(description: &str) -> &str {
    let first = description.split(',').next().unwrap_or_default().trim();
    match first.rsplit_once(' ') {
        Some((name, size)) if size.parse::<f32>().is_ok() => name.trim(),
        _ => first,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_font_descriptions_give_their_family() {
        assert_eq!(family_of("Cantarell 11"), "Cantarell");
        assert_eq!(family_of("Adwaita Sans 11"), "Adwaita Sans");
        assert_eq!(family_of("Noto Sans,10,-1,5,50,0,0,0,0,0"), "Noto Sans");
        assert_eq!(family_of("Inter"), "Inter");
        assert_eq!(family_of(""), "");
    }

    #[test]
    fn cap_drop_has_the_sign_of_where_the_capitals_fall() {
        // Inter: justo en el centro.
        assert!(cap_drop_of(1984.0, -494.0, 1490.0, 2048).abs() < 1e-6);
        // FreeSans (ascendente 900, descendente 300, mayúsculas 729 en 1000): por encima del
        // centro, así que negativo (se compensa con margen arriba).
        let free_sans = cap_drop_of(900.0, -300.0, 729.0, 1000);
        assert!((free_sans + 0.0645).abs() < 1e-4, "{free_sans}");
        // Ascendente grande para sus mayúsculas: caen por debajo, positivo.
        assert!(cap_drop_of(2210.0, -514.0, 1434.0, 2048) > 0.05);
        assert_eq!(cap_drop_of(1.0, -1.0, 1.0, 0), 0.0);
    }

    #[test]
    fn the_embedded_font_is_always_available() {
        assert!(INTER_REGULAR.len() > 10_000 && INTER_SEMIBOLD.len() > 10_000);
        let fonts = Fonts::detect(Some("Una Fuente Que No Existe 12"));
        assert!(!fonts.family.is_empty());
        // Las mayúsculas de Inter caen justo en el centro de la línea (ascendente 1984,
        // descendente 494 y mayúsculas de 1490 en 2048): con ella no hay nada que compensar.
        if fonts.family == EMBEDDED_FAMILY {
            assert!(cap_drop().abs() < 0.001, "Inter: {}", cap_drop());
        }
        assert!(
            cap_drop().abs() < 0.08,
            "una desviación razonable: {}",
            cap_drop()
        );
        assert_eq!(SEMIBOLD.weight, Weight::Semibold);
        assert!(Fonts::default().is_embedded());
    }
}
