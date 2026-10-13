//! Componentes del sistema de diseño, hechos con los *widgets* de `iced` y las clases de
//! [`crate::style`]: tipografía, botones, campos con validación, interruptores, filas de lista con
//! avatar, insignias de estado, avisos en línea, toasts, diálogos modales, progreso con texto,
//! selectores, teclas de atajo, barra lateral, cabecera de página, tarjetas, grupos de ajustes y
//! los estados vacío, cargando y error.

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding, Pixels};
use iced_widget::{
    Column, Row, Space, button, center, column, container, mouse_area, opaque, pick_list,
    progress_bar, row, rule, scrollable, space, stack, text, text_input, toggler, tooltip,
};

use crate::font::{MONO, SEMIBOLD, UI};
use crate::icon::{Icon, icon, icon_with};
use crate::style::{ButtonClass, ContainerClass, InputClass, ProgressClass, SvgClass, TextClass};
use crate::theme::Tone;
use crate::tokens::{size, space as gap, text as type_size};
use crate::{Element, Renderer, Theme};

/// Texto con el tema de coroom.
pub type Text<'a> = iced_widget::Text<'a, Theme, Renderer>;

/// Botón con el tema de coroom.
pub type Button<'a, M> = iced_widget::Button<'a, M, Theme, Renderer>;

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
        }
    }

    fn icon(self) -> SvgClass {
        match self {
            Kind::Primary => SvgClass::OnAccent,
            Kind::Destructive => SvgClass::OnDestructive,
            Kind::Link => SvgClass::Accent,
            Kind::DestructiveFlat => SvgClass::Tone(Tone::Error),
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
        let mut out = column![
            text(field.label)
                .size(type_size::SECONDARY)
                .font(SEMIBOLD)
                .class(TextClass::Secondary),
            input
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
    toggler(value).size(22).on_toggle_maybe(on_toggle).into()
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
    pick_list(options, selected, on_select)
        .padding(Padding::from([8.0, 12.0]))
        .text_size(type_size::BODY)
        .font(UI)
        .into()
}

/// Selector segmentado: `(texto, mensaje, elegido)` por opción.
pub fn segmented<'a, M: Clone + 'a>(options: Vec<(String, M, bool)>) -> Element<'a, M> {
    let buttons = options.into_iter().map(|(label, message, selected)| {
        button(
            text(label)
                .size(type_size::BODY)
                .font(SEMIBOLD)
                .align_x(Horizontal::Center),
        )
        .padding(Padding::from([6.0, 14.0]))
        .class(ButtonClass::Segment { selected })
        .on_press(message)
        .into()
    });
    container(Row::with_children(buttons).spacing(2))
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
    container(
        row![
            dot(tone),
            text(label).size(type_size::CAPTION).font(SEMIBOLD)
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([3.0, 10.0]))
    .class(ContainerClass::Badge(tone))
    .into()
}

/// Punto de estado.
pub fn dot<'a, M: 'a>(tone: Tone) -> Element<'a, M> {
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

/// Los toasts encima de `base`, abajo en el centro; `offset` (0 = en su sitio) los desplaza hacia
/// abajo durante la animación de entrada.
pub fn with_toasts<'a, M: 'a>(
    base: impl Into<Element<'a, M>>,
    toasts: Vec<Element<'a, M>>,
    offset: f32,
) -> Element<'a, M> {
    if toasts.is_empty() {
        return base.into();
    }
    let stacked = Column::with_children(toasts)
        .spacing(gap::XS)
        .align_x(Alignment::Center);
    stack![
        base.into(),
        container(stacked)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Bottom)
            .padding(Padding {
                top: 0.0,
                right: gap::M,
                bottom: (gap::L - offset).max(0.0),
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
/// `Esc` lo hace la aplicación).
pub fn modal<'a, M: Clone + 'a>(
    base: impl Into<Element<'a, M>>,
    dialog: Option<Element<'a, M>>,
    on_dismiss: M,
) -> Element<'a, M> {
    let base = base.into();
    let Some(dialog) = dialog else {
        return base;
    };
    stack![
        base,
        opaque(
            mouse_area(
                center(opaque(dialog))
                    .padding(gap::L)
                    .class(ContainerClass::Scrim)
            )
            .on_press(on_dismiss)
        )
    ]
    .into()
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

/// Página con desplazamiento: contenido centrado hasta el ancho máximo.
pub fn page<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    scrollable(
        container(container(content).max_width(size::CONTENT))
            .padding(Padding::from([gap::L, gap::XL]))
            .center_x(Length::Fill),
    )
    .height(Length::Fill)
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
}
