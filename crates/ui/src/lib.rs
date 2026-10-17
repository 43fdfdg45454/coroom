//! Sistema de diseño de coroom: tokens, tema, iconos y componentes sobre `iced` 0.14.
//!
//! La interfaz debe sentirse **de Linux** (`CLAUDE.md`, "Interfaz de usuario"): la paleta es la de
//! GNOME (libadwaita) en claro y oscuro con el color de acento del sistema, la tipografía la del
//! escritorio y los espaciados y radios los suyos; en Windows, lo mismo con el tema y el acento del
//! sistema. Módulos:
//!
//! - [`animate`]: componentes con movimiento (botones que cambian de estado con una transición,
//!   contenido que se desliza).
//! - [`avatar`]: avatares con imagen (o iniciales) y el indicador de quién habla.
//! - [`tokens`]: espaciado (4-32), radios (8, 12, 16), escala tipográfica, tamaños y movimiento.
//! - [`theme`]: [`theme::Palette`] (los únicos colores fijos del código) con contraste AA
//!   garantizado para cualquier acento, y utilidades de contraste WCAG.
//! - [`style`]: [`Theme`] y los catálogos de estilo de los *widgets* de `iced` con clases propias.
//! - [`font`]: fuente del escritorio o Inter embebida; monoespaciada para códigos y huellas.
//! - [`icon`]: iconos de Lucide (ISC) monocromos.
//! - [`motion`]: animaciones en función del tiempo con la preferencia de movimiento (reducido,
//!   factor de duración), sobre las de `iced`; resortes.
//! - [`release`]: desplegables que eligen al soltar el botón, no al pulsarlo.
//! - [`scroll`]: el scroll de las páginas, con ancla (lo de arriba no empuja lo que se mira) y una
//!   posición por página.
//! - [`slot`]: ranuras que guardan su sitio (capas e insignias siempre presentes, números de ancho
//!   fijo) y texto en una línea con «…».
//! - [`selectable`]: texto que se selecciona con el ratón y se copia (los mensajes del chat).
//! - [`widget`]: componentes (botones, campos, listas, insignias, avisos, toasts, diálogos,
//!   progreso, selectores, atajos, barra lateral, cabecera, tarjetas, estados).
//!
//! No depende de ninguna crate del workspace (`docs/ARQUITECTURA.md` §5): la aplicación le da los datos.

pub mod animate;
pub mod avatar;
pub mod editor;
pub mod font;
pub mod icon;
pub mod motion;
pub mod release;
pub mod scroll;
pub mod selectable;
pub mod slot;
pub mod style;
pub mod tag;
pub mod theme;
pub mod tokens;
pub mod widget;

pub use style::Theme;

/// Renderizador: el de `iced_wgpu` (el mismo `wgpu::Device` que el vídeo).
pub type Renderer = iced_widget::Renderer;

/// Elemento de la interfaz con el tema de coroom.
pub type Element<'a, M> = iced_core::Element<'a, M, Theme, Renderer>;
