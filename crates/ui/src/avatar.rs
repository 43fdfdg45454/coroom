//! Butacas: el avatar de coroom. Cada persona es una butaca (respaldo redondeado arriba, base
//! casi recta) con su foto o, si no tiene, sus iniciales sobre un color estable para su nombre
//! ([`crate::widget::avatar`]). Aquí están la butaca con foto ([`avatar_with`]), con el anillo de
//! quien habla ([`ringed`]) y con la lámpara de estado ([`seat_lamp`]); la fila de butacas de una
//! sala, con las libres en contorno ([`seat_row`]); y el indicador de quién habla sobre el vídeo.
//!
//! La aplicación decodifica cada avatar una vez y le da a [`AvatarImage`] varias copias reducidas
//! a la mitad (256, 128, 64 y 32 px): se dibuja la menor que cubre el tamaño en pantalla con una
//! pantalla HiDPI de factor 2, así una fila de 24 px no reduce de golpe una imagen de 256 px (el
//! filtro lineal sin mipmaps deja dientes).

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::image::Handle;
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Row, Space, container, image, row, stack, text};

use crate::font::{SEMIBOLD, UI};
use crate::style::{ContainerClass, Surface, TextClass, seat_radius};
use crate::theme::Tone;
use crate::tokens::{seat, space, text as type_size};
use crate::widget::{avatar, ellipsize, one_line, seat_outline, slot, with_tooltip};
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

/// Butaca de `name`: su foto recortada a la silueta de la butaca si la hay; si no, sus iniciales
/// en su color.
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
            .border_radius(seat_radius(side))
            .into(),
        None => avatar(theme, name, side),
    }
}

/// Hueco del anillo de «hablando» alrededor de la butaca: el borde de 2 px y 1 px de aire.
pub const RING_PADDING: f32 = 3.0;

/// [`avatar_with`] dentro del anillo de «hablando» si `talking` (con la silueta de la butaca); si
/// no, en el mismo hueco sin anillo, así la fila no se mueve cuando alguien empieza a hablar.
pub fn ringed<'a, M: 'a>(
    theme: &Theme,
    name: &str,
    picture: Option<&AvatarImage>,
    side: f32,
    talking: bool,
) -> Element<'a, M> {
    ring(avatar_with(theme, name, picture, side), side, talking)
}

/// `seat` (una butaca de lado `side`) en el hueco del anillo, con anillo si `talking`.
fn ring<'a, M: 'a>(seat: Element<'a, M>, side: f32, talking: bool) -> Element<'a, M> {
    container(seat)
        .padding(RING_PADDING)
        .class(if talking {
            ContainerClass::SeatRing {
                tone: Tone::Success,
                side,
                gap: RING_PADDING,
            }
        } else {
            ContainerClass::Transparent
        })
        .into()
}

/// [`avatar_with`] con la lámpara de estado: un punto del tono `lamp` en la esquina inferior
/// derecha de la butaca, separado de ella por un filete del color del plano `on` (lo que haya
/// detrás: una tarjeta, el riel, un panel sobre el vídeo). El verde de «salida» dice conectado o
/// enlace directo; el ámbar, relay o conectando; el neutro, desconectado. Con `None` no hay
/// lámpara, pero la butaca ocupa lo mismo y conserva su sitio en el árbol: encenderla no mueve
/// nada. La lámpara cambia de tono con un fundido.
pub fn seat_lamp<'a, M: 'a>(
    theme: &Theme,
    name: &str,
    picture: Option<&AvatarImage>,
    side: f32,
    lamp: Option<Tone>,
    on: Surface,
) -> Element<'a, M> {
    let lamp = lamp.map(|tone| {
        let box_side = seat::lamp(side) + 2.0 * seat::LAMP_GAP;
        crate::animate::ToneFade::new(
            container(Space::new())
                .width(Length::Fixed(box_side))
                .height(Length::Fixed(box_side))
                .class(ContainerClass::SeatLamp { tone, on }),
            tone,
        )
        .into()
    });
    stack![
        avatar_with(theme, name, picture, side),
        container(slot(lamp))
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .align_x(Horizontal::Right)
            .align_y(Vertical::Bottom),
    ]
    .into()
}

/// [`seat_lamp`] dentro del hueco del anillo de «hablando» ([`ringed`]): la butaca de alguien de
/// la sala, con su enlace y con el anillo verde mientras habla.
pub fn ringed_lamp<'a, M: 'a>(
    theme: &Theme,
    name: &str,
    picture: Option<&AvatarImage>,
    side: f32,
    talking: bool,
    lamp: Option<Tone>,
    on: Surface,
) -> Element<'a, M> {
    ring(
        seat_lamp(theme, name, picture, side, lamp, on),
        side,
        talking,
    )
}

// --- Fila de butacas --------------------------------------------------------------------------------

/// Quien ocupa una butaca de [`seat_row`].
#[derive(Debug, Clone, Copy)]
pub struct Seated<'a> {
    /// Nombre visible.
    pub name: &'a str,
    /// Su foto, si tiene y ya llegó.
    pub picture: Option<&'a AvatarImage>,
    /// La lámpara de su enlace o estado, si hay algo que decir.
    pub lamp: Option<Tone>,
    /// Si está hablando (anillo verde).
    pub talking: bool,
}

impl<'a> Seated<'a> {
    /// Alguien sentado, sin foto, lámpara ni anillo.
    pub fn new(name: &'a str) -> Self {
        Self {
            name,
            picture: None,
            lamp: None,
            talking: false,
        }
    }

    /// Con su foto.
    pub fn picture(mut self, picture: Option<&'a AvatarImage>) -> Self {
        self.picture = picture;
        self
    }

    /// Con la lámpara de su estado.
    pub fn lamp(mut self, lamp: Option<Tone>) -> Self {
        self.lamp = lamp;
        self
    }

    /// Hablando o no.
    pub fn talking(mut self, talking: bool) -> Self {
        self.talking = talking;
        self
    }
}

/// Dónde se lee el nombre de cada persona de una [`seat_row`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SeatNames {
    /// En ningún sitio (ya están en una lista al lado).
    Hidden,
    /// Al pasar el ratón por la butaca.
    #[default]
    Tooltip,
    /// Debajo de la butaca, en una línea (con «…» si no cabe).
    Below,
}

/// Cómo es una [`seat_row`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeatRow {
    /// Lado de cada butaca.
    pub side: f32,
    /// Butacas como poco: las que no ocupa nadie se ven libres, en contorno y con su número.
    pub min: usize,
    /// Butacas como mucho (de personas): si hay más, la última dice cuántas faltan («+3»).
    pub max: usize,
    /// Dónde va el nombre.
    pub names: SeatNames,
    /// Sobre qué plano está la fila.
    pub on: Surface,
}

impl Default for SeatRow {
    fn default() -> Self {
        Self {
            side: seat::ROW,
            min: seat::ROW_MIN,
            max: usize::MAX,
            names: SeatNames::default(),
            on: Surface::default(),
        }
    }
}

impl SeatRow {
    /// Fila compacta sobre el vídeo (el panel de la sala del invitado).
    pub fn overlay() -> Self {
        Self {
            side: seat::ROW_SMALL,
            on: Surface::Overlay,
            ..Self::default()
        }
    }

    /// Con butacas de lado `side`.
    pub fn side(mut self, side: f32) -> Self {
        self.side = side;
        self
    }

    /// Con `min` butacas como poco.
    pub fn min(mut self, min: usize) -> Self {
        self.min = min;
        self
    }

    /// Con `max` butacas de personas como mucho.
    pub fn max(mut self, max: usize) -> Self {
        self.max = max;
        self
    }

    /// Con el nombre en `names`.
    pub fn names(mut self, names: SeatNames) -> Self {
        self.names = names;
        self
    }

    /// Sobre el plano `on`.
    pub fn on(mut self, on: Surface) -> Self {
        self.on = on;
        self
    }
}

/// Qué butacas lleva una fila ([`seat_plan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeatPlan {
    /// Personas con butaca propia (las primeras).
    pub shown: usize,
    /// Personas que no caben y van juntas en la última butaca («+N»); 0 si caben todas.
    pub more: usize,
    /// Butacas libres detrás de las ocupadas.
    pub free: usize,
}

impl SeatPlan {
    /// Butacas de la fila, de todas las clases.
    pub fn seats(&self) -> usize {
        self.shown + usize::from(self.more > 0) + self.free
    }

    /// Los números de las butacas libres (las butacas se cuentan desde 1).
    pub fn free_numbers(&self) -> std::ops::Range<usize> {
        let first = self.shown + usize::from(self.more > 0) + 1;
        first..first + self.free
    }
}

/// Reparte `people` personas en una fila con `min` butacas como poco y `max` de personas como
/// mucho: todas con la suya si caben; si no, `max - 1` y la última para el resto. Detrás, libres
/// hasta completar `min`.
pub fn seat_plan(people: usize, min: usize, max: usize) -> SeatPlan {
    let max = max.max(1);
    let (shown, more) = if people <= max {
        (people, 0)
    } else {
        (max - 1, people - (max - 1))
    };
    let taken = shown + usize::from(more > 0);
    SeatPlan {
        shown,
        more,
        free: min.saturating_sub(taken),
    }
}

/// La fila de butacas de una sala: `people` en su orden (quien aloja, primero), cada persona en
/// su butaca con su color o su foto, la lámpara de su enlace y el anillo verde mientras habla; y
/// detrás, butacas libres en contorno con su número hasta completar `options.min`. El nombre va
/// donde diga `options.names`. No se desborda: si no cabe en el ancho que le dan, sigue en otra
/// línea; y con `options.max`, las personas que pasen de ahí se cuentan en una última butaca
/// («+3»). Cada butaca ocupa siempre lo mismo (con o sin anillo, con o sin lámpara), así que
/// nada se mueve cuando alguien habla o cambia su enlace.
pub fn seat_row<'a, M: 'a>(
    theme: &Theme,
    people: &[Seated<'_>],
    options: SeatRow,
) -> Element<'a, M> {
    let SeatRow {
        side,
        min,
        max,
        names,
        on,
    } = options;
    let plan = seat_plan(people.len(), min, max);
    let slot_width = match names {
        SeatNames::Below => seat::LABEL.max(side + 2.0 * RING_PADDING),
        SeatNames::Hidden | SeatNames::Tooltip => side + 2.0 * RING_PADDING,
    };
    let name_class = match on {
        Surface::Overlay => TextClass::OverlaySecondary,
        _ => TextClass::Secondary,
    };
    // Cada butaca en una ranura del mismo ancho, con el nombre debajo si toca.
    let place = |seat: Element<'a, M>, label: Option<String>| -> Element<'a, M> {
        let mut column = Column::new()
            .spacing(space::XXS)
            .width(Length::Fixed(slot_width))
            .align_x(Alignment::Center)
            .push(seat);
        if let Some(label) = label {
            column = column.push(one_line(label, move |content| {
                text(content)
                    .size(type_size::CAPTION)
                    .font(UI)
                    .class(name_class)
            }));
        }
        column.into()
    };
    let mut seats: Vec<Element<'a, M>> = Vec::with_capacity(plan.seats());
    for person in &people[..plan.shown] {
        let seat = ringed_lamp(
            theme,
            person.name,
            person.picture,
            side,
            person.talking,
            person.lamp,
            on,
        );
        seats.push(match names {
            SeatNames::Hidden => place(seat, None),
            SeatNames::Tooltip => place(with_tooltip(seat, person.name.to_owned()), None),
            SeatNames::Below => place(seat, Some(person.name.to_owned())),
        });
    }
    if plan.more > 0 {
        let rest = people[plan.shown..]
            .iter()
            .map(|person| person.name)
            .collect::<Vec<_>>()
            .join(", ");
        let seat: Element<'a, M> = container(seat_outline(format!("+{}", plan.more), side, on))
            .padding(RING_PADDING)
            .into();
        seats.push(place(with_tooltip(seat, rest), None));
    }
    for number in plan.free_numbers() {
        seats.push(place(
            container(seat_outline(number.to_string(), side, on))
                .padding(RING_PADDING)
                .into(),
            None,
        ));
    }
    Row::with_children(seats)
        .spacing(space::XS)
        .align_y(Alignment::Start)
        .wrap()
        .vertical_spacing(space::XS)
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

/// Lado de la butaca en el indicador de quién habla.
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
/// empezó antes), con su butaca rodeada del color de «hablando» y su nombre; si son más de
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
        // Los demás, en una butaca en contorno con la cuenta.
        let count = seat_outline(format!("+{rest}"), SPEAKER_AVATAR, Surface::Overlay);
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
    fn a_row_fills_up_to_its_minimum_with_free_seats() {
        // Nadie: todas libres, numeradas desde la 1.
        let empty = seat_plan(0, 4, usize::MAX);
        assert_eq!((empty.shown, empty.more, empty.free), (0, 0, 4));
        assert_eq!(empty.free_numbers().collect::<Vec<_>>(), [1, 2, 3, 4]);
        // Quien aloja y tres libres.
        let host = seat_plan(1, 4, usize::MAX);
        assert_eq!((host.shown, host.more, host.free), (1, 0, 3));
        assert_eq!(host.free_numbers().collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(host.seats(), 4);
        // Tres: queda una libre, la cuarta.
        let three = seat_plan(3, 4, usize::MAX);
        assert_eq!(three.free_numbers().collect::<Vec<_>>(), [4]);
        // Justo el mínimo o más: ninguna libre y nadie se queda sin butaca.
        for people in [4, 5, 9] {
            let plan = seat_plan(people, 4, usize::MAX);
            assert_eq!((plan.shown, plan.more, plan.free), (people, 0, 0));
            assert_eq!(plan.seats(), people);
            assert!(plan.free_numbers().is_empty());
        }
        // Sin mínimo, solo las ocupadas.
        assert_eq!(seat_plan(2, 0, usize::MAX).seats(), 2);
    }

    #[test]
    fn a_row_with_a_maximum_counts_the_rest_in_its_last_seat() {
        // Caben: cada cual en la suya.
        let fits = seat_plan(6, 4, 6);
        assert_eq!((fits.shown, fits.more, fits.free), (6, 0, 0));
        // No caben: cinco y «+3» (nunca «+1» por alguien que cabría en esa butaca).
        let over = seat_plan(8, 4, 6);
        assert_eq!((over.shown, over.more, over.free), (5, 3, 0));
        assert_eq!(over.seats(), 6);
        let one_over = seat_plan(7, 4, 6);
        assert_eq!((one_over.shown, one_over.more), (5, 2));
        // La butaca de «+N» cuenta para el mínimo y las libres se numeran detrás de ella.
        let narrow = seat_plan(5, 6, 3);
        assert_eq!((narrow.shown, narrow.more, narrow.free), (2, 3, 3));
        assert_eq!(narrow.free_numbers().collect::<Vec<_>>(), [4, 5, 6]);
        // Un máximo de cero no deja la fila sin sitio para decir cuántos hay.
        let none = seat_plan(3, 0, 0);
        assert_eq!((none.shown, none.more), (0, 3));
        assert_eq!(none.seats(), 1);
    }

    #[test]
    fn the_ring_follows_the_seat_at_its_distance() {
        use crate::style::seat_ring_radius;
        for side in [SPEAKER_AVATAR, 36.0, 44.0, 72.0] {
            let seat = seat_radius(side);
            assert_eq!(seat.top_left, seat.top_right);
            assert_eq!(seat.bottom_left, seat.bottom_right);
            assert!((seat.top_left - side * seat::BACK).abs() < 1e-4);
            assert!((seat.bottom_left - side * seat::BASE).abs() < 1e-4);
            let ring = seat_ring_radius(side, RING_PADDING);
            assert!((ring.top_left - seat.top_left - RING_PADDING).abs() < 1e-4);
            assert!((ring.bottom_right - seat.bottom_right - RING_PADDING).abs() < 1e-4);
            // El anillo tampoco llega a ser un círculo.
            assert!(ring.top_left < (side + 2.0 * RING_PADDING) / 2.0);
        }
    }

    #[test]
    fn levels_with_the_wrong_size_are_refused() {
        assert!(AvatarImage::from_rgba_levels(Vec::new()).is_none());
        assert!(AvatarImage::from_rgba_levels(vec![(4, vec![0; 63])]).is_none());
        assert!(AvatarImage::from_rgba_levels(vec![(4, vec![0; 64])]).is_some());
    }
}
