//! Avatares: la imagen de una persona en un círculo o, si no tiene, sus iniciales en un color
//! estable para su nombre ([`crate::widget::avatar`]); y el indicador de quién habla, una fila
//! por persona con su avatar.
//!
//! La aplicación decodifica cada avatar una vez y le da a [`AvatarImage`] varias copias reducidas
//! a la mitad (256, 128, 64 y 32 px): se dibuja la menor que cubre el tamaño en pantalla con una
//! pantalla HiDPI de factor 2, así una fila de 24 px no reduce de golpe una imagen de 256 px (el
//! filtro lineal sin mipmaps deja dientes).

use iced_core::image::Handle;
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, container, image, row, text};

use crate::font::SEMIBOLD;
use crate::style::{ContainerClass, TextClass};
use crate::theme::Tone;
use crate::tokens::{space, text as type_size};
use crate::widget::{avatar, ellipsize};
use crate::{Element, Theme};

/// Imagen de un avatar, en copias de lado decreciente.
#[derive(Debug, Clone, PartialEq)]
pub struct AvatarImage {
    /// `(lado en píxeles, imagen)`, de la mayor a la menor.
    levels: Vec<(u32, Handle)>,
}

impl AvatarImage {
    /// Avatar con estas copias: `(lado, píxeles RGBA)` de la mayor a la menor (cuadradas). `None`
    /// si no hay ninguna o alguna no tiene `lado² × 4` bytes.
    pub fn from_rgba_levels(levels: Vec<(u32, Vec<u8>)>) -> Option<Self> {
        if levels.is_empty()
            || levels
                .iter()
                .any(|(side, pixels)| pixels.len() != (*side as usize).pow(2) * 4)
        {
            return None;
        }
        Some(Self {
            levels: levels
                .into_iter()
                .map(|(side, pixels)| (side, Handle::from_rgba(side, side, pixels)))
                .collect(),
        })
    }

    /// Lado de la copia mayor.
    pub fn side(&self) -> u32 {
        self.levels.first().map_or(0, |(side, _)| *side)
    }

    /// La copia para dibujar en `side` píxeles lógicos: la menor que tiene al menos el doble (o
    /// la mayor, si ninguna llega).
    pub fn for_side(&self, side: f32) -> &Handle {
        let wanted = (side * 2.0).ceil() as u32;
        self.levels
            .iter()
            .rev()
            .find(|(level, _)| *level >= wanted)
            .or_else(|| self.levels.first())
            .map(|(_, handle)| handle)
            .expect("un avatar tiene al menos una copia")
    }
}

/// Avatar redondo: la imagen si la hay; si no, las iniciales de `name` en su color.
pub fn avatar_with<'a, M: 'a>(
    theme: &Theme,
    name: &str,
    picture: Option<&AvatarImage>,
    side: f32,
) -> Element<'a, M> {
    match picture {
        Some(picture) => image(picture.for_side(side).clone())
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .border_radius(side / 2.0)
            .into(),
        None => avatar(theme, name, side),
    }
}

/// Hueco del anillo de «hablando» alrededor del avatar: el borde de 2 px y 1 px de aire.
pub const RING_PADDING: f32 = 3.0;

/// [`avatar_with`] dentro del anillo de «hablando» si `talking`; si no, en el mismo hueco sin
/// anillo, así la fila no se mueve cuando alguien empieza a hablar.
pub fn ringed<'a, M: 'a>(
    theme: &Theme,
    name: &str,
    picture: Option<&AvatarImage>,
    side: f32,
    talking: bool,
) -> Element<'a, M> {
    container(avatar_with(theme, name, picture, side))
        .padding(RING_PADDING)
        .class(if talking {
            ContainerClass::AvatarRing(Tone::Success)
        } else {
            ContainerClass::Transparent
        })
        .into()
}

/// Una persona del indicador de quién habla.
#[derive(Debug, Clone, Copy)]
pub struct Speaker<'a> {
    /// Nombre visible.
    pub name: &'a str,
    /// Su avatar, si tiene y ya llegó.
    pub picture: Option<&'a AvatarImage>,
}

/// Filas como mucho del indicador de quién habla (con la de «y N más»).
pub const SPEAKERS_MAX_ROWS: usize = 4;

/// Lado del avatar en el indicador de quién habla.
pub const SPEAKER_AVATAR: f32 = 22.0;

/// Caracteres como mucho de un nombre en el indicador (el resto, «…»).
pub const SPEAKER_NAME_MAX: usize = 24;

/// Cuántas personas tienen fila propia y cuántas van juntas en «y N más», con `count` hablando
/// y `max` filas: todas si caben; si no, `max - 1` y la última fila para el resto.
pub fn speaker_rows(count: usize, max: usize) -> (usize, usize) {
    if count <= max {
        (count, 0)
    } else {
        let shown = max.saturating_sub(1);
        (shown, count - shown)
    }
}

/// Indicador de quién habla sobre el vídeo: una fila por persona, en el orden dado (el de quien
/// empezó antes), con su avatar rodeado del color de «hablando» y su nombre; si son más de
/// [`SPEAKERS_MAX_ROWS`], la última fila dice cuántos más (`more(n)`). `None` si nadie habla.
pub fn speakers<'a, M: 'a>(
    theme: &Theme,
    people: &[Speaker<'_>],
    more: impl Fn(usize) -> String,
) -> Option<Element<'a, M>> {
    if people.is_empty() {
        return None;
    }
    let (shown, rest) = speaker_rows(people.len(), SPEAKERS_MAX_ROWS);
    let line = |leading: Element<'a, M>, label: String| -> Element<'a, M> {
        row![
            leading,
            text(label)
                .size(type_size::SECONDARY)
                .font(SEMIBOLD)
                .class(TextClass::Overlay)
                .wrapping(Wrapping::None),
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into()
    };
    let mut column = Column::new().spacing(space::XS);
    for person in &people[..shown] {
        let ring = ringed(theme, person.name, person.picture, SPEAKER_AVATAR, true);
        column = column.push(line(
            ring,
            ellipsize(person.name, SPEAKER_NAME_MAX).into_owned(),
        ));
    }
    if rest > 0 {
        let count = container(
            text(format!("+{rest}"))
                .size(type_size::CAPTION)
                .font(SEMIBOLD)
                .class(TextClass::Overlay),
        )
        .center_x(Length::Fixed(SPEAKER_AVATAR))
        .center_y(Length::Fixed(SPEAKER_AVATAR))
        .class(ContainerClass::OverlayBadge(Tone::Neutral));
        column = column.push(line(
            container(count).padding(RING_PADDING).into(),
            more(rest),
        ));
    }
    Some(
        container(column)
            .padding(Padding::from([6.0, 12.0]))
            .class(ContainerClass::Overlay)
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_fit_everyone_or_end_with_how_many_more() {
        assert_eq!(speaker_rows(0, 4), (0, 0));
        assert_eq!(speaker_rows(1, 4), (1, 0));
        assert_eq!(speaker_rows(3, 4), (3, 0), "tres personas, tres filas");
        assert_eq!(speaker_rows(4, 4), (4, 0));
        assert_eq!(speaker_rows(5, 4), (3, 2), "la cuarta fila dice «y 2 más»");
        assert_eq!(speaker_rows(40, 4), (3, 37));
    }

    #[test]
    fn the_smallest_level_that_covers_twice_the_side_is_drawn() {
        let level = |side: u32| (side, vec![0u8; (side * side * 4) as usize]);
        let picture =
            AvatarImage::from_rgba_levels(vec![level(256), level(128), level(64), level(32)])
                .expect("avatar");
        let side_of = |side: f32| {
            let handle = picture.for_side(side);
            picture
                .levels
                .iter()
                .find(|(_, h)| h == handle)
                .map(|(s, _)| *s)
        };
        assert_eq!(side_of(12.0), Some(32));
        assert_eq!(side_of(22.0), Some(64));
        assert_eq!(side_of(32.0), Some(64));
        assert_eq!(side_of(36.0), Some(128));
        assert_eq!(side_of(96.0), Some(256));
        assert_eq!(side_of(200.0), Some(256), "si ninguna llega, la mayor");
        assert_eq!(picture.side(), 256);
    }

    #[test]
    fn initials_keep_their_colour_per_name_and_names_spread_over_the_colours() {
        use crate::theme::{Accent, Mode, Palette};
        let palette = Palette::new(Mode::Light, Accent::default());
        assert_eq!(palette.avatar("Carla"), palette.avatar("Carla"));
        let names = ["Ana", "Bea", "Carla", "Dani", "Eli", "Fran", "Gael", "Hugo"];
        let colours: std::collections::HashSet<_> = names
            .iter()
            .map(|name| format!("{:?}", palette.avatar(name)))
            .collect();
        assert!(
            colours.len() >= 6,
            "ocho nombres cortos, como poco seis colores: {}",
            colours.len()
        );
    }

    #[test]
    fn levels_with_the_wrong_size_are_refused() {
        assert!(AvatarImage::from_rgba_levels(Vec::new()).is_none());
        assert!(AvatarImage::from_rgba_levels(vec![(4, vec![0; 63])]).is_none());
        assert!(AvatarImage::from_rgba_levels(vec![(4, vec![0; 64])]).is_some());
    }
}
