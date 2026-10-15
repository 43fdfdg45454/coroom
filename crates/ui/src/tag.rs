//! Etiquetas de cuándo surte efecto un ajuste: «En vivo» (verde: se aplica al momento, con la
//! sala abierta) y «Al abrir la sala» / «Al entrar en la sala» (gris: se guarda para la próxima).
//!
//! Son píldoras pequeñas sin punto, distintas de las insignias de estado
//! ([`crate::widget::status_badge`]): no dicen cómo está algo, sino cuándo cambia. El texto lo da
//! la aplicación (sus catálogos); aquí solo el aspecto.

use iced_core::Padding;
use iced_core::text::Wrapping;
use iced_widget::{container, text};

use crate::Element;
use crate::font::SEMIBOLD;
use crate::style::ContainerClass;
use crate::theme::Tone;
use crate::tokens::text as type_size;

/// Cuándo surte efecto lo que se cambia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Al momento (verde).
    Live,
    /// En la próxima sala (gris).
    Later,
}

impl When {
    /// Tono de la etiqueta.
    pub fn tone(self) -> Tone {
        match self {
            When::Live => Tone::Success,
            When::Later => Tone::Neutral,
        }
    }
}

/// Etiqueta de cuándo surte efecto un ajuste, con el texto `label`.
pub fn tag<'a, M: 'a>(when: When, label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    container(
        text(label)
            .size(type_size::CAPTION - 1.0)
            .font(SEMIBOLD)
            .wrapping(Wrapping::None),
    )
    .padding(Padding::from([2.0, 8.0]))
    .class(ContainerClass::Badge(when.tone()))
    .into()
}

/// «En vivo».
pub fn live<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    tag(When::Live, label)
}

/// «Al abrir la sala» o «Al entrar en la sala».
pub fn later<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    tag(When::Later, label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_is_green_and_later_is_grey() {
        assert_eq!(When::Live.tone(), Tone::Success);
        assert_eq!(When::Later.tone(), Tone::Neutral);
    }
}
