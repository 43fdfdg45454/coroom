//! Teclas: tecla física de `winit` → código **evdev** (el formato del cable) → *scancode* set 1 de
//! Windows (`SendInput` con `KEYEVENTF_SCANCODE`).
//!
//! Todo sale de una sola lista fija, [`KEYS`], que cubre un teclado completo ANSI e ISO (con la
//! tecla `<>` de ISO, `IntlBackslash`), el bloque numérico, las teclas de edición y flechas con
//! prefijo `E0`, las multimedia más habituales y las de los teclados japonés y coreano que tienen
//! *scancode* propio. Las tablas de búsqueda ([`scancode_from_evdev`] y [`evdev_from_scancode`]) se
//! **generan** a partir de ella en tiempo de compilación; los tests comprueban que no hay códigos
//! repetidos, que la ida y vuelta es exacta y que la lista coincide con la de `winit` en la
//! plataforma en que se compila.
//!
//! *Scancodes*: el byte bajo es el código de *make* del set 1 y `0xE0` en el alto marca las
//! extendidas (`KEYEVENTF_EXTENDEDKEY`). `Pausa` es `0x45` (Windows la sintetiza así; el `E1 1D 45`
//! del cable PS/2 no se puede mandar con `SendInput`) y `Bloq Num` es `0xE045`, como en `winit`.
//!
//! En Android no hay `winit`: allí la tabla es otra, de los `KEYCODE_*` de Android a evdev
//! ([`crate::android`]), y aquí solo quedan los códigos ([`evdev`]) y [`Scancode`].

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
pub use desktop::{
    KEYS, evdev_from_keycode, evdev_from_physical, evdev_from_scancode, keycode_from_evdev,
    keycode_from_name, scancode_from_evdev,
};

/// Códigos evdev (`linux/input-event-codes.h`) que se usan fuera de la tabla.
pub mod evdev {
    /// `KEY_ESC`.
    pub const KEY_ESC: u16 = 1;
    /// `KEY_LEFTCTRL`.
    pub const KEY_LEFTCTRL: u16 = 29;
    /// `KEY_LEFTALT`.
    pub const KEY_LEFTALT: u16 = 56;
    /// `KEY_RIGHTCTRL`.
    pub const KEY_RIGHTCTRL: u16 = 97;
    /// `KEY_RIGHTALT`.
    pub const KEY_RIGHTALT: u16 = 100;
    /// `KEY_G`.
    pub const KEY_G: u16 = 34;
    /// `KEY_O`.
    pub const KEY_O: u16 = 24;
    /// `KEY_Q`.
    pub const KEY_Q: u16 = 16;
    /// `KEY_W`.
    pub const KEY_W: u16 = 17;
    /// `KEY_A`.
    pub const KEY_A: u16 = 30;
    /// `KEY_S`.
    pub const KEY_S: u16 = 31;
    /// `KEY_D`.
    pub const KEY_D: u16 = 32;
    /// `KEY_E`.
    pub const KEY_E: u16 = 18;
    /// `KEY_R`.
    pub const KEY_R: u16 = 19;
    /// `KEY_F`.
    pub const KEY_F: u16 = 33;
    /// `KEY_TAB`.
    pub const KEY_TAB: u16 = 15;
    /// `KEY_ENTER`.
    pub const KEY_ENTER: u16 = 28;
    /// `KEY_SPACE`.
    pub const KEY_SPACE: u16 = 57;
    /// `KEY_LEFTSHIFT`.
    pub const KEY_LEFTSHIFT: u16 = 42;
    /// `KEY_RIGHTSHIFT`.
    pub const KEY_RIGHTSHIFT: u16 = 54;
    /// `KEY_CAPSLOCK`.
    pub const KEY_CAPSLOCK: u16 = 58;
    /// `KEY_NUMLOCK`.
    pub const KEY_NUMLOCK: u16 = 69;
    /// `KEY_SCROLLLOCK`.
    pub const KEY_SCROLLLOCK: u16 = 70;
    /// `KEY_LEFTMETA`.
    pub const KEY_LEFTMETA: u16 = 125;
    /// `KEY_RIGHTMETA`.
    pub const KEY_RIGHTMETA: u16 = 126;
    /// `KEY_UP`.
    pub const KEY_UP: u16 = 103;
    /// `KEY_LEFT`.
    pub const KEY_LEFT: u16 = 105;
    /// `KEY_RIGHT`.
    pub const KEY_RIGHT: u16 = 106;
    /// `KEY_DOWN`.
    pub const KEY_DOWN: u16 = 108;
    /// Mayor código de teclado de la tabla más uno.
    pub const KEY_LIMIT: usize = 256;

    /// `BTN_LEFT`.
    pub const BTN_LEFT: u16 = 0x110;
    /// `BTN_RIGHT`.
    pub const BTN_RIGHT: u16 = 0x111;
    /// `BTN_MIDDLE`.
    pub const BTN_MIDDLE: u16 = 0x112;
    /// `BTN_SIDE` (lateral trasero).
    pub const BTN_SIDE: u16 = 0x113;
    /// `BTN_EXTRA` (lateral delantero).
    pub const BTN_EXTRA: u16 = 0x114;
}

/// *Scancode* set 1: byte bajo el código, `0xE0` en el alto si es extendida.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scancode(pub u16);

impl Scancode {
    /// Código de *make* (sin prefijo).
    pub const fn code(self) -> u16 {
        self.0 & 0x00ff
    }

    /// Si lleva el prefijo `E0` (`KEYEVENTF_EXTENDEDKEY`).
    pub const fn extended(self) -> bool {
        self.0 & 0xff00 == 0xe000
    }
}
