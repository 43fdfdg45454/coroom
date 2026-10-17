//! Etiquetas de cuándo surte efecto un ajuste: «Al momento» (verde: se aplica ya, también con la
//! sala abierta) y «En la próxima sala» / «Al entrar» (gris: se guarda para la próxima).
//!
//! En las filas de Ajustes van en una columna de ancho fijo ([`column`]).
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
use crate::widget::one_line;

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

/// Etiqueta de cuándo surte efecto un ajuste, con el texto `label`: en una línea y, si no cabe,
/// acortada con «…» y entera al pasar el ratón.
pub fn tag<'a, M: 'a>(when: When, label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    container(one_line(label.into_fragment().into_owned(), |label| {
        text(label)
            .size(type_size::CAPTION - 1.0)
            .font(SEMIBOLD)
            .wrapping(Wrapping::None)
    }))
    .padding(Padding::from([2.0, 8.0]))
    .class(ContainerClass::Badge(when.tone()))
    .into()
}

/// «Al momento».
pub fn live<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    tag(When::Live, label)
}

/// «En la próxima sala» o «Al entrar».
pub fn later<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    tag(When::Later, label)
}

/// La columna de «cuándo aplica» de una fila de ajuste ([`crate::widget::setting_row`]): de ancho
/// fijo, el de la más ancha de `labels` (todas las etiquetas que pueden salir en la pantalla,
/// medidas con la letra de verdad), con la de esta fila (`current`) o vacía. Así las etiquetas
/// de las filas quedan en columna y la fila no cambia cuando la etiqueta cambia o no está.
pub fn column<'a, M: 'a>(
    labels: &[(When, &'a str)],
    current: Option<(When, &'a str)>,
) -> Element<'a, M> {
    crate::widget::reserve(
        labels.iter().map(|(when, label)| tag(*when, *label)),
        current.map(|(when, label)| tag(when, label)),
    )
    .into()
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
