//! Estilo del campo de texto de varias líneas (`text_editor` de `iced`): el del chat de la sala,
//! en la ventana (como los campos de una línea) o flotando sobre el vídeo (oscuro translúcido).

use iced_core::border::{self, Border};
use iced_core::{Background, Color};
use iced_widget::text_editor;

use crate::font::Fonts;
use crate::style::Theme;
use crate::theme::{Accent, Mode};
use crate::tokens::radius;

/// Lo que `iced` pide a un tema para los campos de varias líneas (el modo y los colores base).
impl iced_core::theme::Base for Theme {
    fn default(preference: iced_core::theme::Mode) -> Self {
        let mode = match preference {
            iced_core::theme::Mode::Dark => Mode::Dark,
            _ => Mode::Light,
        };
        Theme::new(mode, Accent::Blue, Fonts::default())
    }

    fn mode(&self) -> iced_core::theme::Mode {
        match self.palette.mode {
            Mode::Dark => iced_core::theme::Mode::Dark,
            Mode::Light => iced_core::theme::Mode::Light,
        }
    }

    fn base(&self) -> iced_core::theme::Style {
        iced_core::theme::Style {
            background_color: self.background(),
            text_color: self.text_color(),
        }
    }

    fn palette(&self) -> Option<iced_core::theme::Palette> {
        None
    }

    fn name(&self) -> &str {
        "coroom"
    }
}

/// Clases del campo de varias líneas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorClass {
    /// Sobre la ventana o una tarjeta.
    #[default]
    Normal,
    /// Sobre el vídeo (dentro de un panel `Overlay`).
    Overlay,
    /// Texto que se lee y se selecciona (un mensaje del chat): sin fondo ni borde, del color
    /// del texto de la superficie.
    Reading,
    /// [`EditorClass::Reading`] sobre el vídeo.
    ReadingOverlay,
}

impl text_editor::Catalog for Theme {
    type Class<'a> = EditorClass;

    fn default<'a>() -> Self::Class<'a> {
        EditorClass::Normal
    }

    fn style(&self, class: &Self::Class<'_>, status: text_editor::Status) -> text_editor::Style {
        let p = &self.palette;
        let focused = matches!(status, text_editor::Status::Focused { .. });
        let hovered = matches!(
            status,
            text_editor::Status::Hovered | text_editor::Status::Focused { is_hovered: true }
        );
        let disabled = status == text_editor::Status::Disabled;
        let outline = |color: Color, width: f32| Border {
            color,
            width,
            radius: border::radius(radius::CONTROL),
        };
        match class {
            EditorClass::Normal => text_editor::Style {
                background: Background::Color(if disabled {
                    p.button.color()
                } else {
                    p.card.color()
                }),
                border: if focused {
                    outline(p.focus.color(), 2.0)
                } else if hovered {
                    outline(p.text_secondary.alpha(0.6), 1.0)
                } else {
                    outline(p.border.color(), 1.0)
                },
                placeholder: p.text_secondary.color(),
                value: if disabled {
                    p.text_disabled.color()
                } else {
                    p.text.color()
                },
                selection: p.accent_bg.alpha(0.35),
            },
            EditorClass::Reading | EditorClass::ReadingOverlay => {
                let overlay = *class == EditorClass::ReadingOverlay;
                text_editor::Style {
                    background: Background::Color(Color::TRANSPARENT),
                    border: outline(Color::TRANSPARENT, 0.0),
                    placeholder: Color::TRANSPARENT,
                    value: if overlay {
                        p.overlay_text.color()
                    } else {
                        p.text.color()
                    },
                    selection: p.accent_bg.alpha(if overlay { 0.45 } else { 0.35 }),
                }
            }
            EditorClass::Overlay => text_editor::Style {
                background: Background::Color(p.overlay_text.alpha(if disabled {
                    0.04
                } else {
                    0.08
                })),
                border: if focused {
                    outline(p.focus.color(), 2.0)
                } else if hovered {
                    outline(p.overlay_text.alpha(0.28), 1.0)
                } else {
                    outline(p.overlay_text.alpha(0.14), 1.0)
                },
                placeholder: p.overlay_text_secondary.color(),
                value: if disabled {
                    p.overlay_text_secondary.color()
                } else {
                    p.overlay_text.color()
                },
                selection: p.accent_bg.alpha(0.45),
            },
        }
    }
}
