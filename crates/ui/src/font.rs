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

use iced_core::Font;
use iced_core::font::Weight;

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
    fn the_embedded_font_is_always_available() {
        assert!(INTER_REGULAR.len() > 10_000 && INTER_SEMIBOLD.len() > 10_000);
        let fonts = Fonts::detect(Some("Una Fuente Que No Existe 12"));
        assert!(!fonts.family.is_empty());
        assert_eq!(SEMIBOLD.weight, Weight::Semibold);
        assert!(Fonts::default().is_embedded());
    }
}
