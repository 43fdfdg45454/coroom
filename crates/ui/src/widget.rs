//! Componentes del sistema de diseño, hechos con los *widgets* de `iced` y las clases de
//! [`crate::style`]: tipografía, botones, campos con validación, interruptores, filas de lista con
//! avatar, insignias de estado, avisos en línea, toasts, diálogos modales, progreso con texto,
//! selectores, deslizadores, teclas de atajo, barra lateral, cabecera de página, tarjetas,
//! grupos de ajustes y los estados vacío, cargando y error.

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding, Pixels};
use iced_widget::{
    Column, Row, Space, center, column, container, mouse_area, opaque, pick_list, progress_bar,
    row, rule, scrollable, space, stack, text, text_input, tooltip,
};

use crate::animate::button;
use crate::font::{MONO, SEMIBOLD, UI};
use crate::icon::{Icon, icon, icon_with};
use crate::release::release_to_pick;
use crate::style::{
    ButtonClass, ContainerClass, InputClass, ProgressClass, SliderClass, SvgClass, TextClass,
};
use crate::theme::Tone;
use crate::tokens::{size, space as gap, text as type_size};
use crate::{Element, Renderer, Theme};

/// Texto con el tema de coroom.
pub type Text<'a> = iced_widget::Text<'a, Theme, Renderer>;

/// Botón con el tema de coroom, animado ([`crate::animate`]).
pub type Button<'a, M> = crate::animate::AnimatedButton<'a, M>;

// --- Tipografía ---------------------------------------------------------------------------------

/// Título de página (24/600).
pub fn title<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content).size(type_size::TITLE).font(SEMIBOLD)
}

/// Encabezado de sección (18/600).
pub fn heading<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content).size(type_size::HEADING).font(SEMIBOLD)
}

/// Cuerpo (14/400).
pub fn body<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content).size(type_size::BODY).font(UI)
}

/// Cuerpo en semibold (títulos de filas y tarjetas).
pub fn strong<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content).size(type_size::BODY).font(SEMIBOLD)
}

/// Secundario (13/400, color secundario).
pub fn secondary<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content)
        .size(type_size::SECONDARY)
        .font(UI)
        .class(TextClass::Secondary)
}

/// Pie (12, color secundario).
pub fn caption<'a>(content: impl text::IntoFragment<'a>) -> Text<'a> {
    text(content)
        .size(type_size::CAPTION)
        .font(UI)
        .class(TextClass::Secondary)
}

/// Monoespaciada (códigos y huellas).
pub fn mono<'a>(content: impl text::IntoFragment<'a>, size: f32) -> Text<'a> {
    text(content).size(size).font(MONO)
}

// --- Botones ------------------------------------------------------------------------------------

/// Tipo de botón.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Acción principal.
    Primary,
    /// Normal.
    Normal,
    /// Plano.
    Flat,
    /// Plano con el acento (enlace).
    Link,
    /// Destructivo.
    Destructive,
    /// Plano destructivo.
    DestructiveFlat,
    /// Destructivo suave (fondo rojo tenue): lo peligroso que no es cerrar ni salir.
    DestructiveSoft,
}

impl Kind {
    fn class(self) -> ButtonClass {
        match self {
            Kind::Primary => ButtonClass::Primary,
            Kind::Normal => ButtonClass::Normal,
            Kind::Flat => ButtonClass::Flat,
            Kind::Link => ButtonClass::Link,
            Kind::Destructive => ButtonClass::Destructive,
            Kind::DestructiveFlat => ButtonClass::DestructiveFlat,
            Kind::DestructiveSoft => ButtonClass::DestructiveSoft,
        }
    }

    fn icon(self) -> SvgClass {
        match self {
            Kind::Primary => SvgClass::OnAccent,
            Kind::Destructive => SvgClass::OnDestructive,
            Kind::Link => SvgClass::Accent,
            Kind::DestructiveFlat | Kind::DestructiveSoft => SvgClass::Tone(Tone::Error),
            Kind::Normal | Kind::Flat => SvgClass::Text,
        }
    }
}

/// Botón con texto (y un icono delante si se da). Sin `on_press` queda desactivado.
pub fn button_with<'a, M: Clone + 'a>(
    kind: Kind,
    label: impl text::IntoFragment<'a>,
    leading: Option<Icon>,
    on_press: Option<M>,
) -> Button<'a, M> {
    let label = text(label).size(type_size::BODY).font(SEMIBOLD);
    let content: Element<'a, M> = match leading {
        Some(leading) => row![icon_with(leading, size::ICON, kind.icon()), label]
            .spacing(gap::XS)
            .align_y(Alignment::Center)
            .into(),
        None => label.into(),
    };
    button(container(content).center_y(Length::Fixed(size::CONTROL - 16.0)))
        .padding(Padding::from([8.0, 16.0]))
        .class(kind.class())
        .on_press_maybe(on_press)
}

/// Botón principal.
pub fn primary<'a, M: Clone + 'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<M>,
) -> Button<'a, M> {
    button_with(Kind::Primary, label, None, on_press)
}

/// Botón normal.
pub fn normal<'a, M: Clone + 'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<M>,
) -> Button<'a, M> {
    button_with(Kind::Normal, label, None, on_press)
}

/// Botón plano.
pub fn flat<'a, M: Clone + 'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<M>,
) -> Button<'a, M> {
    button_with(Kind::Flat, label, None, on_press)
}

/// Botón destructivo.
pub fn destructive<'a, M: Clone + 'a>(
    label: impl text::IntoFragment<'a>,
    on_press: Option<M>,
) -> Button<'a, M> {
    button_with(Kind::Destructive, label, None, on_press)
}

/// Botón de solo icono con su descripción como texto emergente (y para la accesibilidad).
pub fn icon_button<'a, M: Clone + 'a>(
    glyph: Icon,
    description: impl text::IntoFragment<'a>,
    on_press: Option<M>,
) -> Element<'a, M> {
    let control = button(center(icon(glyph, size::ICON)))
        .width(Length::Fixed(size::CONTROL))
        .height(Length::Fixed(size::CONTROL))
        .padding(0)
        .class(ButtonClass::Flat)
        .press_scale(true)
        .on_press_maybe(on_press);
    with_tooltip(control, description)
}

/// `content` con un texto emergente debajo.
pub fn with_tooltip<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    tip: impl text::IntoFragment<'a>,
) -> Element<'a, M> {
    tooltip(
        content,
        container(text(tip).size(type_size::CAPTION).font(UI))
            .padding(Padding::from([4.0, 8.0]))
            .class(ContainerClass::Toast),
        tooltip::Position::Bottom,
    )
    .gap(4)
    .into()
}

// --- Campos ---------------------------------------------------------------------------------------

/// Campo de texto con etiqueta y validación: con `error`, el borde se pone rojo y el motivo se
/// lee debajo (en español, con la causa).
#[derive(Debug, Clone)]
pub struct Field<'a, M> {
    label: &'a str,
    placeholder: &'a str,
    value: std::borrow::Cow<'a, str>,
    on_input: Option<fn(String) -> M>,
    on_submit: Option<M>,
    error: Option<&'a str>,
    hint: Option<&'a str>,
    secure: bool,
    id: Option<iced_widget::Id>,
    monospace: bool,
}

/// Empieza un campo.
pub fn field<'a, M: Clone + 'a>(
    label: &'a str,
    value: impl Into<std::borrow::Cow<'a, str>>,
) -> Field<'a, M> {
    Field {
        label,
        placeholder: "",
        value: value.into(),
        on_input: None,
        on_submit: None,
        error: None,
        hint: None,
        secure: false,
        id: None,
        monospace: false,
    }
}

impl<'a, M: Clone + 'a> Field<'a, M> {
    /// Texto de ejemplo.
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    /// Mensaje al escribir (sin él, desactivado).
    pub fn on_input(mut self, on_input: fn(String) -> M) -> Self {
        self.on_input = Some(on_input);
        self
    }

    /// Mensaje al pulsar Intro.
    pub fn on_submit(mut self, message: Option<M>) -> Self {
        self.on_submit = message;
        self
    }

    /// Error de validación.
    pub fn error(mut self, error: Option<&'a str>) -> Self {
        self.error = error;
        self
    }

    /// Ayuda bajo el campo (si no hay error).
    pub fn hint(mut self, hint: Option<&'a str>) -> Self {
        self.hint = hint;
        self
    }

    /// Contraseña.
    pub fn secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    /// Identificador (para el foco con `Tab`).
    pub fn id(mut self, id: impl Into<iced_widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Valor en monoespaciada (códigos).
    pub fn monospace(mut self) -> Self {
        self.monospace = true;
        self
    }
}

impl<'a, M: Clone + 'a> From<Field<'a, M>> for Element<'a, M> {
    fn from(field: Field<'a, M>) -> Self {
        let mut input = text_input(field.placeholder, &field.value)
            .padding(Padding::from([8.0, 12.0]))
            .size(type_size::BODY)
            .font(if field.monospace { MONO } else { UI })
            .secure(field.secure)
            .class(if field.error.is_some() {
                InputClass::Invalid
            } else {
                InputClass::Normal
            });
        if let Some(on_input) = field.on_input {
            input = input.on_input(on_input);
        }
        if let Some(message) = field.on_submit {
            input = input.on_submit(message);
        }
        if let Some(id) = field.id {
            input = input.id(id);
        }
        let error = field.error.map(|error| {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            error.hash(&mut hasher);
            hasher.finish()
        });
        let mut out = column![
            text(field.label)
                .size(type_size::SECONDARY)
                .font(SEMIBOLD)
                .class(TextClass::Secondary),
            crate::animate::Shake::new(input, error)
        ]
        .spacing(6);
        if let Some(error) = field.error {
            out = out.push(
                row![
                    icon_with(Icon::CircleAlert, 14.0, SvgClass::Tone(Tone::Error)),
                    text(error)
                        .size(type_size::SECONDARY)
                        .class(TextClass::Tone(Tone::Error))
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        } else if let Some(hint) = field.hint {
            out = out.push(caption(hint));
        }
        out.into()
    }
}

/// Interruptor (sin `on_toggle`, desactivado).
pub fn switch<'a, M: Clone + 'a>(
    value: bool,
    on_toggle: Option<impl Fn(bool) -> M + 'a>,
) -> Element<'a, M> {
    crate::animate::Switch::new(value, on_toggle, 22.0).into()
}

/// Deslizador de coroom.
pub type Slider<'a, M> = iced_widget::Slider<'a, u8, M, Theme>;

/// Ancho de la etiqueta del valor de [`slider`] («200 %» cabe sin mover el raíl).
const SLIDER_LABEL: f32 = 48.0;

/// Deslizador de un valor entero pequeño (porcentajes, niveles) con su valor a la derecha:
/// `label` es el valor ya escrito para quien lo lee («150 %»); `icon`, si se da, lo precede.
/// `on_change` llega mientras se arrastra (también con las flechas, de `step` en `step`) y
/// `on_release` al soltar: para guardar. Con Ctrl pulsado, el tirador vuelve a `default`.
/// `class` lo adapta al fondo (ventana o panel sobre el vídeo); el raíl ocupa lo que le dejen.
#[allow(clippy::too_many_arguments)]
pub fn slider<'a, M: Clone + 'a>(
    range: std::ops::RangeInclusive<u8>,
    value: u8,
    step: u8,
    default: u8,
    icon: Option<Icon>,
    label: impl text::IntoFragment<'a>,
    class: SliderClass,
    on_change: impl Fn(u8) -> M + 'a,
    on_release: Option<M>,
) -> Element<'a, M> {
    let icon_class = match class {
        SliderClass::Accent | SliderClass::AccentMuted => SvgClass::Secondary,
        SliderClass::Overlay | SliderClass::OverlayMuted => SvgClass::Overlay,
    };
    let leading = icon.map(|glyph| icon_with(glyph, size::ICON, icon_class));
    slider_with(
        range, value, step, default, leading, label, class, on_change, on_release,
    )
}

/// [`slider`] con lo que se quiera delante del raíl en vez de un icono fijo (por ejemplo, un
/// botón que silencia lo que el deslizador regula).
#[allow(clippy::too_many_arguments)]
pub fn slider_with<'a, M: Clone + 'a>(
    range: std::ops::RangeInclusive<u8>,
    value: u8,
    step: u8,
    default: u8,
    leading: Option<Element<'a, M>>,
    label: impl text::IntoFragment<'a>,
    class: SliderClass,
    on_change: impl Fn(u8) -> M + 'a,
    on_release: Option<M>,
) -> Element<'a, M> {
    let mut control: Slider<'a, M> = iced_widget::Slider::new(range, value, on_change)
        .step(step)
        .shift_step(1)
        .default(default)
        .height(size::CONTROL - 12.0)
        .class(class);
    if let Some(on_release) = on_release {
        control = control.on_release(on_release);
    }
    let text_class = match class {
        SliderClass::Accent | SliderClass::AccentMuted => TextClass::Secondary,
        SliderClass::Overlay | SliderClass::OverlayMuted => TextClass::OverlaySecondary,
    };
    let mut line = Row::new().spacing(gap::XS).align_y(Alignment::Center);
    if let Some(leading) = leading {
        line = line.push(leading);
    }
    line.push(control.width(Length::Fill))
        .push(
            text(label)
                .size(type_size::SECONDARY)
                .font(UI)
                .class(text_class)
                .width(Length::Fixed(SLIDER_LABEL))
                .align_x(Horizontal::Right),
        )
        .into()
}

/// Selector desplegable.
pub fn select<'a, T, M>(
    options: impl std::borrow::Borrow<[T]> + 'a,
    selected: Option<T>,
    on_select: impl Fn(T) -> M + 'a,
) -> Element<'a, M>
where
    T: ToString + PartialEq + Clone + 'a,
    M: Clone + 'a,
{
    release_to_pick(
        pick_list(options, selected, on_select)
            .padding(Padding::from([8.0, 12.0]))
            .text_size(type_size::BODY)
            .font(UI),
    )
    .into()
}

/// Menú de acciones de una fila: un botón «`label` ▾» que despliega `actions` (su texto, con
/// `Display`) y llama a `on_action` con la elegida. No guarda selección: siempre enseña `label`.
pub fn menu<'a, T, M>(
    actions: Vec<T>,
    label: &'a str,
    on_action: impl Fn(T) -> M + 'a,
) -> Element<'a, M>
where
    T: ToString + PartialEq + Clone + 'a,
    M: Clone + 'a,
{
    release_to_pick(
        pick_list(actions, None::<T>, on_action)
            .placeholder(label)
            .padding(Padding::from([6.0, 10.0]))
            .text_size(type_size::SECONDARY)
            .font(UI),
    )
    .into()
}

/// Selector desplegable de ancho fijo, para opciones de texto largo (títulos de ventana): sin él,
/// el desplegable toma el ancho de la opción más larga y aplasta el resto de la fila. Las opciones
/// deberían venir ya recortadas ([`ellipsize`]) para caber.
pub fn select_fixed<'a, T, M>(
    options: impl std::borrow::Borrow<[T]> + 'a,
    selected: Option<T>,
    on_select: impl Fn(T) -> M + 'a,
    width: f32,
) -> Element<'a, M>
where
    T: ToString + PartialEq + Clone + 'a,
    M: Clone + 'a,
{
    release_to_pick(
        pick_list(options, selected, on_select)
            .padding(Padding::from([8.0, 12.0]))
            .text_size(type_size::BODY)
            .font(UI)
            .width(Length::Fixed(width)),
    )
    .into()
}

/// `text` con como mucho `max` caracteres: si sobra, lo corta y termina en «…».
pub fn ellipsize(text: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if text.chars().count() <= max {
        return text.into();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end()).into()
}

/// Selector segmentado: `(texto, mensaje, elegido)` por opción. El fondo del elegido se desliza
/// hasta el nuevo ([`crate::animate::Segments`]).
pub fn segmented<'a, M: Clone + 'a>(options: Vec<(String, M, bool)>) -> Element<'a, M> {
    let chosen = options.iter().position(|(_, _, selected)| *selected);
    let buttons = options.into_iter().map(|(label, message, selected)| {
        button(
            text(label)
                .size(type_size::BODY)
                .font(SEMIBOLD)
                .wrapping(Wrapping::None)
                .align_x(Horizontal::Center),
        )
        .padding(Padding::from([6.0, 14.0]))
        .class(ButtonClass::Segment { selected })
        .on_press(message)
        .into()
    });
    container(crate::animate::Segments::new(
        Row::with_children(buttons).spacing(2),
        chosen,
        ButtonClass::SegmentThumb,
    ))
    .padding(3)
    .class(ContainerClass::Segmented)
    .into()
}

// --- Listas ---------------------------------------------------------------------------------------

/// Iniciales de un nombre («Ana García» → «AG», «bea» → «B»).
pub fn initials(name: &str) -> String {
    let mut out: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// Avatar redondo con las iniciales de `name` y un color estable para ese nombre.
pub fn avatar<'a, M: 'a>(theme: &Theme, name: &str, side: f32) -> Element<'a, M> {
    let (background, text_color) = theme.palette.avatar(name);
    container(
        text(initials(name))
            .size((side * 0.4).round())
            .font(SEMIBOLD),
    )
    .center_x(Length::Fixed(side))
    .center_y(Length::Fixed(side))
    .class(ContainerClass::Avatar {
        background,
        text: text_color,
    })
    .into()
}

/// Fila de lista: algo delante (avatar o icono), título, subtítulo y acciones al final.
pub fn list_row<'a, M: 'a>(
    leading: Option<Element<'a, M>>,
    title_text: impl text::IntoFragment<'a>,
    subtitle: Option<String>,
    trailing: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let mut texts = column![strong(title_text).wrapping(Wrapping::WordOrGlyph)].spacing(2);
    if let Some(subtitle) = subtitle {
        texts = texts.push(secondary(subtitle).wrapping(Wrapping::WordOrGlyph));
    }
    let mut line = Row::new().spacing(gap::S).align_y(Alignment::Center);
    if let Some(leading) = leading {
        line = line.push(leading);
    }
    line = line.push(texts.width(Length::Fill));
    for element in trailing {
        line = line.push(element);
    }
    container(line)
        .padding(Padding::from([10.0, 16.0]))
        .width(Length::Fill)
        .into()
}

/// Lista agrupada en una tarjeta con separadores (la `boxed-list` de GNOME).
pub fn boxed_list<'a, M: 'a>(rows: Vec<Element<'a, M>>) -> Element<'a, M> {
    let mut list = Column::new();
    let count = rows.len();
    for (index, element) in rows.into_iter().enumerate() {
        list = list.push(element);
        if index + 1 < count {
            list = list.push(rule::horizontal(1));
        }
    }
    container(list)
        .width(Length::Fill)
        .class(ContainerClass::Card)
        .into()
}

/// Insignia de estado: punto del tono y texto.
pub fn status_badge<'a, M: 'a>(tone: Tone, label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    // En una línea: si no cabe, «…» y el texto entero al pasar el ratón.
    status_badge_with(
        tone,
        one_line(label.into_fragment().into_owned(), badge_text),
    )
}

/// [`status_badge`] con lo que se quiera tras el punto (un texto y una cifra de ancho fijo, por
/// ejemplo, escritos con [`badge_text`]).
pub fn status_badge_with<'a, M: 'a>(
    tone: Tone,
    content: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    crate::animate::ToneFade::new(
        container(
            row![dot_shape(tone), content.into()]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .padding(Padding::from([3.0, 10.0]))
        .class(ContainerClass::Badge(tone)),
        tone,
    )
    .into()
}

/// El texto de las insignias de estado (12/600).
pub fn badge_text<'a>(content: String) -> Text<'a> {
    text(content).size(type_size::CAPTION).font(SEMIBOLD)
}

/// Punto de estado (cambia de tono con un fundido).
pub fn dot<'a, M: 'a>(tone: Tone) -> Element<'a, M> {
    crate::animate::ToneFade::new(dot_shape(tone), tone).into()
}

/// El punto sin fundido propio (dentro de algo que ya se funde).
fn dot_shape<'a, M: 'a>(tone: Tone) -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fixed(size::DOT))
        .height(Length::Fixed(size::DOT))
        .class(ContainerClass::Dot(tone))
        .into()
}

// --- Avisos ---------------------------------------------------------------------------------------

fn tone_icon(tone: Tone) -> Icon {
    match tone {
        Tone::Info | Tone::Neutral => Icon::Info,
        Tone::Success => Icon::CircleCheck,
        Tone::Warning => Icon::Warning,
        Tone::Error => Icon::CircleAlert,
    }
}

/// Aviso en línea (información, aviso o error) con una acción opcional.
pub fn banner<'a, M: Clone + 'a>(
    tone: Tone,
    message: impl text::IntoFragment<'a>,
    action: Option<(&'a str, M)>,
) -> Element<'a, M> {
    let mut line = row![
        icon_with(tone_icon(tone), 18.0, SvgClass::Tone(tone)),
        text(message)
            .size(type_size::BODY)
            .width(Length::Fill)
            .wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(gap::S)
    .align_y(Alignment::Center);
    if let Some((label, message)) = action {
        line = line.push(
            button(text(label).size(type_size::BODY).font(SEMIBOLD))
                .padding(Padding::from([6.0, 12.0]))
                .class(ButtonClass::Flat)
                .on_press(message),
        );
    }
    container(line)
        .padding(Padding::from([10.0, 14.0]))
        .width(Length::Fill)
        .class(ContainerClass::Banner(tone))
        .into()
}

/// Toast: aviso breve abajo en el centro. `shown` (0 → 1) es el avance de su animación de
/// entrada: sube desde abajo.
pub fn toast<'a, M: 'a>(tone: Tone, message: impl text::IntoFragment<'a>) -> Element<'a, M> {
    let svg = match tone {
        Tone::Neutral => SvgClass::Overlay,
        tone => SvgClass::Tone(tone),
    };
    container(
        row![
            icon_with(tone_icon(tone), 18.0, svg),
            text(message).size(type_size::BODY)
        ]
        .spacing(gap::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([10.0, 16.0]))
    .max_width(560)
    .class(ContainerClass::Toast)
    .into()
}

/// Los toasts encima de `base`, abajo a la derecha, el más nuevo abajo. Cada uno viene ya con su
/// movimiento ([`crate::animate`]: entra desde abajo y sale hacia la derecha plegándose, y los
/// demás se deslizan a su sitio).
pub fn with_toasts<'a, M: 'a>(
    base: impl Into<Element<'a, M>>,
    toasts: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    // Siempre la misma forma (pila de dos capas), con o sin toasts: si la raíz cambiara de tipo,
    // iced tiraría el estado de toda la interfaz (el desplazamiento de la página, el foco) cada
    // vez que aparece o se va un toast.
    let stacked = Column::with_children(toasts).align_x(Alignment::End);
    stack![
        base.into(),
        container(stacked)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Right)
            .align_y(Vertical::Bottom)
            .padding(Padding {
                top: 0.0,
                right: gap::L,
                bottom: gap::L - gap::XS,
                left: gap::M,
            })
    ]
    .into()
}

// --- Diálogos -------------------------------------------------------------------------------------

/// Diálogo: título, cuerpo y acciones a la derecha.
pub fn dialog<'a, M: 'a>(
    title_text: impl text::IntoFragment<'a>,
    content: impl Into<Element<'a, M>>,
    actions: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    container(
        column![
            heading(title_text),
            content.into(),
            Row::with_children(actions)
                .spacing(gap::XS)
                .width(Length::Fill)
                .align_y(Alignment::Center)
        ]
        .spacing(gap::M),
    )
    .padding(gap::L)
    .max_width(size::NARROW)
    .class(ContainerClass::Dialog)
    .into()
}

/// `base` con `dialog` encima, sobre un velo; pulsar fuera del diálogo envía `on_dismiss` (y
/// `Esc` lo hace la aplicación). `shown` (0 a 1, ya con su curva) es lo que lleva de su entrada:
/// el velo se oscurece y el diálogo sube hasta su sitio desde `slide` px más abajo (el texto no se
/// escala: cambiaría el rasterizado de la letra en cada cuadro).
pub fn modal<'a, M: Clone + 'a>(
    base: impl Into<Element<'a, M>>,
    dialog: Option<Element<'a, M>>,
    on_dismiss: M,
    shown: f32,
    slide: f32,
) -> Element<'a, M> {
    // Misma forma con o sin diálogo (ver [`with_toasts`]): abrir o cerrar el diagnóstico no debe
    // devolver la página al principio.
    let overlay: Element<'a, M> = match dialog {
        Some(dialog) => opaque(
            mouse_area(
                center(opaque(crate::animate::Shift::new(
                    dialog,
                    iced_core::Vector::new(0.0, slide * (1.0 - shown)),
                )))
                .padding(gap::L)
                .class(ContainerClass::Scrim(shown)),
            )
            .on_press(on_dismiss),
        ),
        None => Space::new().width(0).height(0).into(),
    };
    stack![base.into(), overlay].into()
}

// --- Progreso y estados ---------------------------------------------------------------------------

/// Progreso con su texto (nunca un indicador sin explicar). `value` en `[0, 1]`; `None` es
/// indeterminado y se anima con `phase` (`[0, 1)`, la aplicación la hace avanzar).
pub fn progress<'a, M: 'a>(
    label: impl text::IntoFragment<'a>,
    value: Option<f32>,
    phase: f32,
) -> Element<'a, M> {
    let value = value.unwrap_or_else(|| {
        // Indeterminado: crece y vuelve a empezar.
        (phase.fract() * 1.2).min(1.0)
    });
    column![
        secondary(label),
        progress_bar(0.0..=1.0, value)
            .girth(Length::Fixed(4.0))
            .class(ProgressClass::Accent)
    ]
    .spacing(6)
    .into()
}

/// Estado vacío: icono, título, explicación y una acción.
pub fn empty_state<'a, M: 'a>(
    glyph: Icon,
    title_text: impl text::IntoFragment<'a>,
    explanation: impl text::IntoFragment<'a>,
    action: Option<Element<'a, M>>,
) -> Element<'a, M> {
    let mut content = column![
        icon_with(glyph, 40.0, SvgClass::Secondary),
        strong(title_text),
        secondary(explanation)
            .align_x(Horizontal::Center)
            .wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(gap::XS)
    .align_x(Alignment::Center)
    .max_width(420);
    if let Some(action) = action {
        content = content.push(Space::new().height(Length::Fixed(gap::XS)));
        content = content.push(action);
    }
    container(content)
        .padding(gap::L)
        .center_x(Length::Fill)
        .into()
}

/// Estado de error: qué pasó, por qué y qué hacer.
pub fn error_state<'a, M: Clone + 'a>(
    title_text: impl text::IntoFragment<'a>,
    cause: impl text::IntoFragment<'a>,
    action: Option<(&'a str, M)>,
) -> Element<'a, M> {
    let mut content = column![
        icon_with(Icon::CircleAlert, 40.0, SvgClass::Tone(Tone::Error)),
        strong(title_text),
        secondary(cause)
            .align_x(Horizontal::Center)
            .wrapping(Wrapping::WordOrGlyph)
    ]
    .spacing(gap::XS)
    .align_x(Alignment::Center)
    .max_width(460);
    if let Some((label, message)) = action {
        content = content.push(Space::new().height(Length::Fixed(gap::XS)));
        content = content.push(button_with(
            Kind::Normal,
            label,
            Some(Icon::Refresh),
            Some(message),
        ));
    }
    container(content)
        .padding(gap::L)
        .center_x(Length::Fill)
        .into()
}

/// Esqueleto de carga: `rows` filas grises con un texto que dice qué se carga.
pub fn skeleton<'a, M: 'a>(label: impl text::IntoFragment<'a>, rows: usize) -> Element<'a, M> {
    let widths = [0.55, 0.38, 0.47, 0.3];
    let lines = (0..rows).map(|index| {
        let width = widths[index % widths.len()];
        row![
            container(Space::new())
                .width(Length::Fixed(size::AVATAR))
                .height(Length::Fixed(size::AVATAR))
                .class(ContainerClass::Skeleton),
            column![
                container(Space::new())
                    .width(Length::FillPortion((width * 100.0) as u16))
                    .height(Length::Fixed(12.0))
                    .class(ContainerClass::Skeleton),
                container(Space::new())
                    .width(Length::FillPortion((width * 60.0) as u16))
                    .height(Length::Fixed(10.0))
                    .class(ContainerClass::Skeleton),
            ]
            .spacing(6)
            .width(Length::Fill)
        ]
        .spacing(gap::S)
        .align_y(Alignment::Center)
        .padding(Padding::from([10.0, 16.0]))
        .into()
    });
    column![
        caption(label),
        container(Column::with_children(lines))
            .width(Length::Fill)
            .class(ContainerClass::Card)
    ]
    .spacing(gap::XS)
    .into()
}

// --- Atajos ---------------------------------------------------------------------------------------

/// Teclas de un atajo («Ctrl», «,»).
pub fn shortcut<'a, M: 'a>(keys: &[&'a str]) -> Element<'a, M> {
    let caps = keys.iter().map(|key| {
        container(text(*key).size(type_size::CAPTION).font(SEMIBOLD))
            .padding(Padding::from([1.0, 6.0]))
            .class(ContainerClass::Keycap)
            .into()
    });
    Row::with_children(caps)
        .spacing(gap::XXS)
        .align_y(Alignment::Center)
        .into()
}

// --- Estructura -----------------------------------------------------------------------------------

/// Entrada de la barra lateral.
#[derive(Debug, Clone)]
pub struct NavItem<M> {
    /// Icono.
    pub icon: Icon,
    /// Texto.
    pub label: String,
    /// Si es la página actual.
    pub selected: bool,
    /// Al pulsar.
    pub on_press: M,
    /// Contador (solicitudes pendientes…).
    pub badge: Option<String>,
}

/// Barra lateral: cabecera, entradas y pie (usuario y servidor).
pub fn sidebar<'a, M: Clone + 'a>(
    header: Element<'a, M>,
    items: Vec<NavItem<M>>,
    footer: Element<'a, M>,
) -> Element<'a, M> {
    let entries = items.into_iter().map(|item| {
        let mut line = row![
            icon(item.icon, 18.0),
            text(item.label)
                .size(type_size::BODY)
                .font(if item.selected { SEMIBOLD } else { UI })
                .width(Length::Fill)
        ]
        .spacing(gap::S)
        .align_y(Alignment::Center);
        if let Some(badge) = item.badge {
            line = line.push(status_badge(Tone::Info, badge));
        }
        button(line)
            .padding(Padding::from([8.0, 12.0]))
            .width(Length::Fill)
            .class(ButtonClass::Nav {
                selected: item.selected,
            })
            .on_press(item.on_press)
            .into()
    });
    container(
        column![
            header,
            Column::with_children(entries).spacing(2),
            space::vertical(),
            footer
        ]
        .spacing(gap::M)
        .height(Length::Fill),
    )
    .padding(gap::S)
    .width(Length::Fixed(size::SIDEBAR))
    .height(Length::Fill)
    .class(ContainerClass::Sidebar)
    .into()
}

/// Cabecera de página: título, subtítulo y acciones a la derecha.
pub fn page_header<'a, M: 'a>(
    title_text: impl text::IntoFragment<'a>,
    subtitle: Option<String>,
    actions: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let mut texts = column![title(title_text)].spacing(4);
    if let Some(subtitle) = subtitle {
        texts = texts.push(secondary(subtitle));
    }
    row![
        texts.width(Length::Fill),
        Row::with_children(actions)
            .spacing(gap::XS)
            .align_y(Alignment::Center)
    ]
    .spacing(gap::M)
    .align_y(Alignment::Center)
    .into()
}

/// Página con desplazamiento: contenido centrado hasta el ancho máximo. `key` dice qué página es
/// (cada una recuerda su posición) y lo de arriba no empuja lo que se está mirando
/// ([`crate::scroll`]).
pub fn page<'a, M: 'a>(
    key: impl std::hash::Hash,
    content: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    use std::hash::{DefaultHasher, Hasher};

    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    crate::scroll::Anchored::new(
        hasher.finish(),
        scrollable(
            container(container(content).max_width(size::CONTENT))
                .padding(Padding::from([gap::L, gap::XL]))
                .center_x(Length::Fill),
        )
        .height(Length::Fill),
    )
    .into()
}

/// Tarjeta.
pub fn card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(content)
        .padding(gap::M)
        .width(Length::Fill)
        .class(ContainerClass::Card)
        .into()
}

/// Fila de tarjetas igual de altas: la de la más alta. Una fila de iced no iguala alturas (una
/// tarjeta con `Length::Fill` en una fila que se encoge se queda en cero), así que va en una pila:
/// debajo, `cards(false)` dentro de [`Measure`], que solo da la altura (con `Length::Shrink`; ni se
/// dibuja ni recibe eventos ni foco); encima, `cards(true)`, las de verdad con `Length::Fill`.
pub fn equal_height_row<'a, M: 'a>(
    cards: impl Fn(bool) -> Vec<Element<'a, M>>,
    spacing: f32,
) -> Element<'a, M> {
    stack![
        Measure {
            content: Row::with_children(cards(false))
                .spacing(spacing)
                .height(Length::Shrink)
                .into(),
        },
        Row::with_children(cards(true))
            .spacing(spacing)
            .height(Length::Fill),
    ]
    .into()
}

/// Ocupa lo que ocuparía su contenido sin dibujarlo, sin eventos y sin operaciones (foco, `id`).
struct Measure<'a, M> {
    content: Element<'a, M>,
}

impl<'a, M: 'a> iced_core::Widget<M, Theme, Renderer> for Measure<'a, M> {
    fn size(&self) -> iced_core::Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut iced_core::widget::Tree,
        renderer: &Renderer,
        limits: &iced_core::layout::Limits,
    ) -> iced_core::layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        _tree: &iced_core::widget::Tree,
        _renderer: &mut Renderer,
        _theme: &Theme,
        _style: &iced_core::renderer::Style,
        _layout: iced_core::Layout<'_>,
        _cursor: iced_core::mouse::Cursor,
        _viewport: &iced_core::Rectangle,
    ) {
    }

    fn children(&self) -> Vec<iced_core::widget::Tree> {
        vec![iced_core::widget::Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut iced_core::widget::Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }
}

impl<'a, M: 'a> From<Measure<'a, M>> for Element<'a, M> {
    fn from(measure: Measure<'a, M>) -> Self {
        Element::new(measure)
    }
}

/// Tarjeta entera pulsable.
pub fn card_button<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    on_press: Option<M>,
) -> Element<'a, M> {
    button(content)
        .padding(gap::M)
        .width(Length::Fill)
        .class(ButtonClass::Card)
        .on_press_maybe(on_press)
        .into()
}

/// Grupo de ajustes con título (el `preferences-group` de GNOME).
pub fn section<'a, M: 'a>(
    title_text: impl text::IntoFragment<'a>,
    description: Option<String>,
    rows: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let mut header = column![strong(title_text)].spacing(2);
    if let Some(description) = description {
        header = header.push(secondary(description).wrapping(Wrapping::WordOrGlyph));
    }
    column![header, boxed_list(rows)].spacing(gap::XS).into()
}

/// Fila de ajuste: título, subtítulo y el control a la derecha.
pub fn pref_row<'a, M: 'a>(
    title_text: impl text::IntoFragment<'a>,
    subtitle: Option<String>,
    control: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    let mut texts = column![body(title_text)].spacing(2);
    if let Some(subtitle) = subtitle {
        texts = texts.push(secondary(subtitle).wrapping(Wrapping::WordOrGlyph));
    }
    row![texts.width(Length::Fill), control.into()]
        .spacing(gap::M)
        .align_y(Alignment::Center)
        .padding(Padding::from([10.0, 16.0]))
        .into()
}

/// [`pref_row`] con lo que se quiera bajo el título (texto de ayuda y un aviso, por ejemplo).
pub fn pref_row_with<'a, M: 'a>(
    title_text: impl text::IntoFragment<'a>,
    below: impl Into<Element<'a, M>>,
    control: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    let texts = column![body(title_text), below.into()].spacing(2);
    row![texts.width(Length::Fill), control.into()]
        .spacing(gap::M)
        .align_y(Alignment::Center)
        .padding(Padding::from([10.0, 16.0]))
        .into()
}

/// Valor monoespaciado con un botón de copiar.
pub fn copy_field<'a, M: Clone + 'a>(value: &'a str, on_copy: M) -> Element<'a, M> {
    row![
        container(mono(value, type_size::SECONDARY).wrapping(Wrapping::WordOrGlyph))
            .padding(Padding::from([8.0, 12.0]))
            .width(Length::Fill)
            .class(ContainerClass::Code),
        icon_button(Icon::Copy, "Copiar", Some(on_copy))
    ]
    .spacing(gap::XS)
    .align_y(Alignment::Center)
    .into()
}

/// Espacio fijo.
pub fn gap<'a, M: 'a>(pixels: f32) -> Element<'a, M> {
    Space::new()
        .width(Length::Fixed(pixels))
        .height(Length::Fixed(pixels))
        .into()
}

/// Espacio que empuja lo siguiente al final de una fila.
pub fn fill<'a, M: 'a>() -> Element<'a, M> {
    space::horizontal().into()
}

// --- Ranuras --------------------------------------------------------------------------------------

/// Hijo que puede no estar: `content` o, sin él, un hueco que guarda su posición entre los hijos
/// (un `Space` que se encoge, no uno de 0×0, que filas, columnas y pilas descartan). Así lo que va
/// después no cambia de índice y conserva su estado (scroll, foco, arrastre). No reserva tamaño:
/// para eso, [`reserved`] o [`reserve`]. En una fila o columna con `spacing`, el hueco también
/// lleva el espaciado; donde moleste, que el espacio lo ponga lo mostrado (un `padding`).
pub fn slot<'a, M: 'a>(content: Option<Element<'a, M>>) -> Element<'a, M> {
    content.unwrap_or_else(|| Space::new().into())
}

/// Ranura de tamaño fijo, `width` × `height`, con `content` centrado o vacía: mostrar u ocultar
/// un icono o un botón de tamaño conocido no mueve lo de alrededor.
pub fn reserved<'a, M: 'a>(
    width: f32,
    height: f32,
    content: Option<Element<'a, M>>,
) -> Element<'a, M> {
    container(slot(content))
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .center_x(Length::Fixed(width))
        .center_y(Length::Fixed(height))
        .into()
}

/// Ranura del tamaño de la mayor de `samples` (cada variante de una insignia, por ejemplo), con
/// `content` o vacía: la mide el renderizador al maquetar, con la letra de verdad
/// ([`crate::slot::Reserve`]). Alineada a la izquierda; `.align_x` para otra cosa.
pub fn reserve<'a, M: 'a>(
    samples: impl IntoIterator<Item = Element<'a, M>>,
    content: Option<Element<'a, M>>,
) -> crate::slot::Reserve<'a, M> {
    crate::slot::Reserve::new(samples, content)
}

/// Número de ancho fijo: `value` escrito con `style`, alineado a la derecha en una ranura del
/// ancho de la más ancha de `widest` escrita igual («888 ms», «88,8 Mbit/s»). El ancho no se
/// calcula con métricas supuestas de la letra: el renderizador maqueta cada muestra con el mismo
/// estilo y la ranura toma la mayor ([`reserve`]); `iced` no permite pedir cifras tabulares, así
/// que con letra proporcional conviene que las muestras lleven la cifra más ancha (8) en cada
/// posición. Si `value` no cabe (no debería), la ranura crece hasta él.
pub fn figure<'a, M: 'a>(
    style: impl Fn(String) -> Text<'a>,
    value: impl Into<String>,
    widest: &[&str],
) -> Element<'a, M> {
    figure_aligned(style, value, widest, Horizontal::Right)
}

/// [`figure`] alineada a la izquierda: para una cifra que sigue a un texto en la misma línea
/// («servidor · 12 ms»), donde el hueco de la ranura queda al final y no en medio.
pub fn figure_after<'a, M: 'a>(
    style: impl Fn(String) -> Text<'a>,
    value: impl Into<String>,
    widest: &[&str],
) -> Element<'a, M> {
    figure_aligned(style, value, widest, Horizontal::Left)
}

fn figure_aligned<'a, M: 'a>(
    style: impl Fn(String) -> Text<'a>,
    value: impl Into<String>,
    widest: &[&str],
    align: Horizontal,
) -> Element<'a, M> {
    let line =
        |content: String| -> Element<'a, M> { style(content).wrapping(Wrapping::None).into() };
    reserve(
        widest.iter().map(|sample| line((*sample).to_owned())),
        Some(line(value.into())),
    )
    .align_x(align)
    .into()
}

/// Texto en una línea escrito con `style`: si no cabe en lo que le dejan, se acorta con «…»
/// (medido con el renderizador) y enseña `full` al pasar el ratón ([`crate::slot::OneLine`]).
/// Para píldoras, etiquetas y líneas de datos que no deben cortar línea ni cambiar de alto.
pub fn one_line<'a, M: 'a>(
    full: impl Into<String>,
    style: impl Fn(String) -> Text<'a> + 'a,
) -> crate::slot::OneLine<'a, M> {
    crate::slot::OneLine::new(full, style)
}

/// Tamaño de texto en [`Pixels`] (atajo).
pub fn px(value: f32) -> Pixels {
    Pixels(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_two_words() {
        assert_eq!(initials("Ana García"), "AG");
        assert_eq!(initials("bea"), "B");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("Carla de la Vega"), "CD");
    }

    #[test]
    fn ellipsize_cuts_long_text_only() {
        assert_eq!(ellipsize("Juego", 10), "Juego");
        assert_eq!(
            ellipsize("Fatal Fury: City of the Wolves", 12),
            "Fatal Fury:…"
        );
        assert_eq!(ellipsize("ñandú ñandú", 6), "ñandú…");
    }
}
