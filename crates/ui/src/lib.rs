//! Sistema de diseño de coroom: tokens, tema, iconos y componentes sobre `iced` 0.14.
//!
//! La interfaz debe sentirse **de Linux** (`CLAUDE.md`, "Interfaz de usuario"): la paleta es la de
//! GNOME (libadwaita) en claro y oscuro con el color de acento del sistema, la tipografía la del
//! escritorio y los espaciados y radios los suyos; en Windows, lo mismo con el tema y el acento del
//! sistema. Módulos:
//!
//! - [`tokens`]: espaciado (4-32), radios (8, 12, 16), escala tipográfica, tamaños y duraciones.
//! - [`theme`]: [`theme::Palette`] (los únicos colores fijos del código) con contraste AA
//!   garantizado para cualquier acento, y utilidades de contraste WCAG.
//! - [`style`]: [`Theme`] y los catálogos de estilo de los *widgets* de `iced` con clases propias.
//! - [`font`]: fuente del escritorio o Inter embebida; monoespaciada para códigos y huellas.
//! - [`icon`]: iconos de Lucide (ISC) monocromos.
//! - [`widget`]: componentes (botones, campos, listas, insignias, avisos, toasts, diálogos,
//!   progreso, selectores, atajos, barra lateral, cabecera, tarjetas, estados).
//!
//! No depende de ninguna crate del workspace (`docs/ARQUITECTURA.md` §5): la aplicación le da los datos.

pub mod editor;
pub mod font;
pub mod icon;
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
