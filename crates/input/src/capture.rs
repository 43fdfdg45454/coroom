//! Captura en el invitado: eventos de la ventana `winit` → [`InputEvent`].
//!
//! [`InputCapture`] no toca la ventana: devuelve [`Action`] que la ventana aplica (enviar un evento,
//! capturar o soltar el cursor, mostrar u ocultar el overlay, salir), así se prueba sin pantalla.
//!
//! - **Modo escritorio** (por defecto): cursor libre; su posición dentro del vídeo (descontando las
//!   bandas negras) va como [`InputEvent::MouseMoveAbsolute`] en `0..=65535` del fotograma del
//!   anfitrión.
//! - **Modo inmersivo**: la ventana captura el cursor (`CursorGrabMode::Locked`, o `Confined` y
//!   recentrado si el compositor no admite `Locked`) y lo oculta; el movimiento va como
//!   [`InputEvent::MouseMove`] con los deltas crudos de `DeviceEvent::MouseMotion`. Si el cursor se
//!   suelta (foco perdido), un clic en la ventana lo vuelve a capturar (ese clic no se envía).
//! - **Clic para capturar** (como QEMU): con el permiso de teclado y ratón recién dado no se
//!   reenvía nada hasta que el invitado hace clic en el vídeo; ese clic no llega al anfitrión.
//!   `Ctrl+Alt+R` ([`ShortcutKeys::release`]) y perder el foco sueltan el reenvío (el permiso
//!   sigue; ver [`InputCapture::forwarding`]) y otro clic lo recupera. Al soltarlo se envía
//!   [`InputEvent::ReleaseAll`]. Los mandos no dependen de esto: no chocan con el escritorio del
//!   invitado y siguen yendo mientras haya ranura.
//! - **Teclado** por tecla física ([`crate::keymap`]); las repeticiones automáticas no se envían
//!   (el sistema del anfitrión repite solo).
//! - **Rueda** en 1/120 de muesca: `LineDelta` × 120 y `PixelDelta` a [`WHEEL_PIXELS_PER_NOTCH`]
//!   píxeles por muesca, guardando los restos.
//! - **Atajos** (no se envían al anfitrión): `Ctrl+Alt+G` alterna inmersivo y escritorio (entrar
//!   en inmersivo captura el reenvío); `Ctrl+Alt+R` suelta el reenvío; `Ctrl+Alt+S` muestra u
//!   oculta el overlay; `Ctrl+Alt+C` el chat; `Ctrl+Alt+Q` sale; `Ctrl+Alt+M` silencia o reactiva
//!   el micrófono, `Ctrl+Alt+V` el audio del juego y `Ctrl+Alt+J` muestra u oculta el panel de
//!   mandos. Sin permiso de teclado, `Esc` también sale.
//! - **AltGr no es Alt**: el Alt de un atajo es el izquierdo o el que el sistema da por Alt
//!   ([`InputCapture::set_modifiers`]). Con una distribución con AltGr (latinoamericana,
//!   española, alemana…) el Alt derecho es AltGr: en Linux llega como `AltRight` sin modificador
//!   Alt (es `ISO_Level3_Shift`) y en Windows como `AltRight` con un `ControlLeft` sintético que
//!   `winit` descarta y sin Ctrl ni Alt en los modificadores. Así `AltGr+Q` (`@`) va al juego y no
//!   sale de la sala; con una distribución sin AltGr, `Ctrl+Alt derecho` sigue valiendo.
//! - **Pulsar para hablar** ([`InputCapture::set_push_to_talk`]): la tecla elegida
//!   (`--ptt TECLA`) no va al anfitrión; pulsarla y soltarla dan [`Action::PushToTalk`].
//! - **Foco**: al perderlo se envía [`InputEvent::ReleaseAll`] y se suelta el cursor.
//!
//! Solo se envía lo que el anfitrión permite ([`Allowed`]); sin teclado y ratón, nada.
//!
//! Lo que depende de `winit` ([`InputCapture`], [`ShortcutKeys`]) no se compila en Android; los
//! permisos, el modo, las [`Action`] y la cuenta de las bandas negras ([`VideoRect`]) valen en
//! todas las plataformas (el tacto del móvil, [`crate::touch`], usa las mismas).

use crate::InputEvent;

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
pub use desktop::{InputCapture, ShortcutKeys, mouse_button};

/// Píxeles de desplazamiento suave (panel táctil) que cuentan como una muesca de rueda.
pub const WHEEL_PIXELS_PER_NOTCH: f64 = 40.0;

/// Lo que el anfitrión permite a este invitado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Allowed {
    /// Teclado y ratón.
    pub keyboard_mouse: bool,
    /// Ranura de mando.
    pub gamepad_slot: Option<u8>,
}

impl Allowed {
    /// Texto del overlay: "solo mirar", "teclado y ratón", "mando en ranura 2" o los dos.
    pub fn label(&self) -> String {
        match (self.keyboard_mouse, self.gamepad_slot) {
            (false, None) => "solo mirar".to_owned(),
            (true, None) => "teclado y ratón".to_owned(),
            (false, Some(slot)) => format!("mando en ranura {slot}"),
            (true, Some(slot)) => format!("teclado y ratón, mando en ranura {slot}"),
        }
    }
}

/// Modo del ratón.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CaptureMode {
    /// Absoluto, cursor libre.
    #[default]
    Desktop,
    /// Relativo, cursor capturado.
    Immersive,
}

impl CaptureMode {
    /// Nombre para el overlay.
    pub const fn label(self) -> &'static str {
        match self {
            CaptureMode::Desktop => "escritorio",
            CaptureMode::Immersive => "inmersivo",
        }
    }
}

/// Lo que la ventana debe hacer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Enviar el evento al anfitrión.
    Send(InputEvent),
    /// Capturar (`true`) o soltar el cursor.
    Grab(bool),
    /// Mostrar u ocultar el overlay.
    ToggleOverlay,
    /// Mostrar u ocultar el chat de la sala (`Ctrl+Alt+C`).
    ToggleChat,
    /// Cerrar la ventana.
    Quit,
    /// Silenciar o reactivar el micrófono (`Ctrl+Alt+M`).
    ToggleMicMute,
    /// Silenciar o reactivar el audio del juego (`Ctrl+Alt+V`).
    ToggleGameMute,
    /// Mostrar u ocultar el panel de mandos (`Ctrl+Alt+J`).
    TogglePads,
    /// La tecla de pulsar para hablar se pulsó (`true`) o se soltó.
    PushToTalk(bool),
}

/// Dónde cae el vídeo dentro de la superficie que lo muestra (la ventana, la vista del móvil), en
/// sus píxeles: el fotograma del anfitrión escalado sin deformarlo y centrado, con bandas negras a
/// los lados o arriba y abajo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoRect {
    /// Borde izquierdo.
    pub x: f64,
    /// Borde superior.
    pub y: f64,
    /// Ancho (mayor que 0).
    pub width: f64,
    /// Alto (mayor que 0).
    pub height: f64,
}

impl VideoRect {
    /// El de un fotograma `frame` (ancho y alto del anfitrión) dentro de una superficie `view`.
    pub fn fit(frame: (u32, u32), view: (u32, u32)) -> Self {
        let (fw, fh) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
        let (tw, th) = (f64::from(view.0.max(1)), f64::from(view.1.max(1)));
        // Escalado por el lado que limita, con ese lado exacto (sin el error de redondeo de
        // multiplicar por la escala).
        let (width, height) = if tw * fh <= th * fw {
            (tw, fh * tw / fw)
        } else {
            (fw * th / fh, th)
        };
        Self {
            x: (tw - width) / 2.0,
            y: (th - height) / 2.0,
            width,
            height,
        }
    }

    /// Posición `(x, y)` de la superficie como [`InputEvent::MouseMoveAbsolute`] en `0..=65535`
    /// del fotograma; fuera del vídeo (en las bandas), la del borde más cercano.
    pub fn absolute(&self, x: f64, y: f64) -> InputEvent {
        let norm = |value: f64, origin: f64, size: f64| {
            let fraction = ((value - origin) / size.max(f64::MIN_POSITIVE)).clamp(0.0, 1.0);
            // NaN (una coordenada inválida) queda en 0.
            (fraction * 65535.0).round() as u16
        };
        InputEvent::MouseMoveAbsolute {
            x: norm(x, self.x, self.width),
            y: norm(y, self.y, self.height),
        }
    }
}

/// Parte entera de `rest + value`, guardando el resto.
pub(crate) fn whole(rest: &mut f64, value: f64) -> i32 {
    *rest += value;
    let out = rest.trunc();
    *rest -= out;
    out.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_video_is_letterboxed_by_the_limiting_side() {
        // Más ancha que el vídeo: bandas a los lados; más alta: arriba y abajo.
        let wide = VideoRect::fit((1920, 1080), (2400, 1080));
        assert_eq!(
            (wide.x, wide.y, wide.width, wide.height),
            (240.0, 0.0, 1920.0, 1080.0)
        );
        let tall = VideoRect::fit((1920, 1080), (1080, 2400));
        assert_eq!((tall.x, tall.width), (0.0, 1080.0));
        assert_eq!(tall.height, 607.5);
        assert_eq!(tall.y, (2400.0 - 607.5) / 2.0);
        // Fuera del vídeo, al borde; una coordenada inválida, a 0.
        let at = |x, y| wide.absolute(x, y);
        assert_eq!(
            at(1200.0, 540.0),
            InputEvent::MouseMoveAbsolute { x: 32768, y: 32768 }
        );
        assert_eq!(
            at(0.0, 2000.0),
            InputEvent::MouseMoveAbsolute { x: 0, y: 65535 }
        );
        assert_eq!(
            at(f64::NAN, 0.0),
            InputEvent::MouseMoveAbsolute { x: 0, y: 0 }
        );
        // Sin tamaño, no divide por cero.
        let empty = VideoRect::fit((0, 0), (0, 0));
        assert_eq!((empty.width, empty.height), (1.0, 1.0));
    }
}
