//! La aplicación de escritorio: `coroom` sin argumentos.
//!
//! - [`app`]: estado, mensajes y efectos; puro (se prueba y se captura sin ventana).
//! - [`screens`]: las pantallas (servidor y entrada, inicio, amigos, anfitrión, invitado,
//!   ajustes; diagnóstico es una sección de ajustes).
//! - [`focus`]: `Tab` y `Mayús+Tab` con vuelta al principio (accesibilidad).
//! - [`backend`]: ejecuta los efectos (API, sesiones, ajustes) en hilos y avisa con mensajes.
//! - [`runner`]: la ventana (`winit` + `iced_winit` + `iced_wgpu` con el dispositivo del vídeo).
//! - [`video`]: el vídeo como widget de `iced` (fotogramas por canal desde la sesión).
//! - [`settings`]: `settings.toml` en el directorio de datos.
//! - [`system`]: tema claro u oscuro, acento y fuente del sistema.
//! - [`stats`]: cifras en español para el panel de estadísticas.
//! - [`shots`]: capturas de cada pantalla fuera de pantalla, con datos de ejemplo.
//!
//! El sistema de diseño (colores, tipografía, iconos, componentes) está en `coroom-ui`.

pub mod app;
pub mod backend;
pub mod focus;
pub mod runner;
pub mod screens;
pub mod settings;
pub mod shots;
pub mod stats;
pub mod system;
pub mod video;

pub use runner::{Launch, run};
