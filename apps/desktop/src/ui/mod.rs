//! La aplicación de escritorio: `coroom` sin argumentos.
//!
//! - [`app`]: estado, mensajes y efectos; puro (se prueba y se captura sin ventana).
//! - [`screens`]: las pantallas (servidor y entrada, inicio, amigos, anfitrión, invitado,
//!   ajustes; diagnóstico es una sección de ajustes).
//! - [`focus`]: `Tab` y `Mayús+Tab` con vuelta al principio (accesibilidad).
//! - [`avatars`]: los avatares en memoria, los que faltan por pedir y el propio en Ajustes.
//! - [`backend`]: ejecuta los efectos (API, sesiones, ajustes) en hilos y avisa con mensajes.
//! - [`runner`]: la ventana (`winit` + `iced_winit` + `iced_wgpu` con el dispositivo del vídeo).
//! - [`compose`]: el compositor de la ventana: el vídeo se entrega en cuanto llega y la interfaz,
//!   dibujada en su textura solo cuando cambia, va encima.
//! - [`video`]: el vídeo como widget de `iced` (fotogramas por canal desde la sesión).
//! - [`pacing`]: el ritmo de los cuadros de animación (lo marca el compositor) y lo que tarda en
//!   dibujarse la interfaz.
//! - [`settings`]: `settings.toml` en el directorio de datos.
//! - [`notify`]: qué aviso con sonido suena con cada evento, y cómo se juntan los seguidos.
//! - [`sound`]: la reproducción de los avisos (por la salida elegida, fuera de la sala) y el
//!   selector de un tono propio.
//! - [`tones`]: los tonos propios de los avisos, sintetizados e igualados en sonoridad.
//! - [`system`]: tema claro u oscuro, acento y fuente del sistema.
//! - [`stats`]: cifras en español para el panel de estadísticas.
//! - [`shots`]: capturas de cada pantalla fuera de pantalla, con datos de ejemplo.
//!
//! El sistema de diseño (colores, tipografía, iconos, componentes) está en `coroom-ui`.

pub mod app;
pub mod avatars;
pub mod backend;
pub mod compose;
pub mod focus;
pub mod notify;
pub mod pacing;
pub mod runner;
pub mod screens;
pub mod settings;
pub mod shots;
pub mod sound;
pub mod stats;
pub mod system;
pub mod tones;
pub mod video;

pub use runner::{Launch, run};
