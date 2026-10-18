//! Entrada de coroom: teclado, ratón y mandos del invitado inyectados en el anfitrión.
//!
//! - **Modelo**: los eventos y su formato en el cable son los de [`coroom_proto::input`]
//!   (reexportados aquí): teclas por código **evdev**, ratón relativo y absoluto, rueda en 1/120 y
//!   mandos con el vocabulario de Xbox, por ranura.
//! - **Invitado**: [`capture::InputCapture`] traduce los eventos de la ventana `winit` (modos
//!   inmersivo y escritorio, atajos), [`gamepad::GamepadCapture`] lee los mandos con `gilrs` a
//!   250 Hz y [`send::InputSender`] agrupa, cifra con la clave pareja a pareja
//!   ([`coroom_crypto::input`]) y envía por los dos canales. [`script`] reproduce una secuencia
//!   JSON sin ventana (tests y la puerta de 1000 Hz).
//! - **Anfitrión**: el trait [`InputSink`] ([`inject`]) con los backends de cada plataforma tras su
//!   `feature`: **libei** por el portal `RemoteDesktop` y **uinput** para los mandos en Linux
//!   (`linux-portal`), `SendInput` en Windows (`windows-sendinput`) con los mandos por el driver
//!   propio de UMDF2 (`windows-vhid`); y [`RecordingSink`] para los
//!   tests. La sesión aplica permisos y pausa **antes** de llamar al sumidero.
//! - [`keymap`]: tecla física de `winit` → evdev → *scancode* set 1 de Windows.
//! - **Invitado de Android** (la aplicación en Kotlin pasa los eventos por JNI como números):
//!   [`touch`] convierte el tacto en ratón (panel o directo) y [`android`] las teclas a evdev y los
//!   mandos (convertidos) a sus ranuras, con la vibración hacia la aplicación. Los dos son
//!   independientes de la plataforma y se prueban en el escritorio. En Android no se compila lo
//!   que depende de `winit` y `gilrs` ([`capture::InputCapture`], la tabla de [`keymap`],
//!   [`gamepad::GamepadCapture`]); lo demás que usa la sesión (permisos, [`gamepad::PadLink`],
//!   [`send`], [`inject`], que allí es [`NullSink`]) vale igual.

use std::fmt;

pub mod android;
pub mod capture;
pub mod gamepad;
#[cfg(all(target_os = "linux", feature = "linux-portal"))]
pub mod hidraw;
#[cfg(target_os = "windows")]
pub mod hidwin;
pub mod inject;
pub mod keymap;
pub mod script;
pub mod send;
pub mod touch;

pub use coroom_proto::input::{
    EventClass, GamepadAxis, GamepadButton, GamepadKind, InputEvent, MouseButton,
};
pub use inject::{InputSink, NullSink, Recorded, RecordingSink};

/// Identificador de un invitado dentro de la sala (el de la señalización).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GuestId(pub u32);

/// Estado de los dos motores de vibración de un mando (el de `FF_RUMBLE` de Linux y el de
/// `XINPUT_VIBRATION`): vale hasta el siguiente, y [`Rumble::OFF`] los para.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rumble {
    /// Motor grande (baja frecuencia), de 0 a 65 535.
    pub strong: u16,
    /// Motor pequeño (alta frecuencia), de 0 a 65 535.
    pub weak: u16,
}

impl Rumble {
    /// Los dos motores parados.
    pub const OFF: Rumble = Rumble { strong: 0, weak: 0 };

    /// Con esas intensidades.
    pub const fn new(strong: u16, weak: u16) -> Self {
        Self { strong, weak }
    }

    /// Si algún motor gira.
    pub const fn is_on(self) -> bool {
        self.strong != 0 || self.weak != 0
    }
}

/// Error de la entrada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// El invitado no tiene permiso para este tipo de entrada.
    NotPermitted(GuestId),
    /// El usuario no concedió el permiso del sistema (por ejemplo, el portal `RemoteDesktop`).
    PermissionDenied,
    /// Todavía no hay dispositivo (por ejemplo, esperando a que el usuario acepte el portal).
    NotReady(String),
    /// No hay mando virtual (Windows sin el driver `coroomvhid` instalado).
    GamepadUnavailable(String),
    /// La ranura de mando no existe o no está asignada.
    NoSuchSlot(u8),
    /// No se puede crear el dispositivo `uinput` (permisos de `/dev/uinput`).
    Uinput(String),
    /// El backend no está compilado o no existe en esta plataforma.
    Unsupported(String),
    /// Error interno del backend.
    Backend(String),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputError::NotPermitted(guest) => {
                write!(
                    f,
                    "el invitado {} no tiene permiso para esa entrada",
                    guest.0
                )
            }
            InputError::PermissionDenied => {
                f.write_str("el permiso de entrada del sistema se denegó (portal RemoteDesktop)")
            }
            InputError::NotReady(what) => write!(f, "la entrada aún no está lista: {what}"),
            InputError::GamepadUnavailable(what) => {
                write!(f, "mando virtual no disponible: {what}")
            }
            InputError::NoSuchSlot(slot) => write!(f, "la ranura de mando {slot} no está asignada"),
            InputError::Uinput(what) => write!(f, "{what}"),
            InputError::Unsupported(what) => write!(f, "entrada no disponible: {what}"),
            InputError::Backend(what) => write!(f, "error del backend de entrada: {what}"),
        }
    }
}

impl std::error::Error for InputError {}

/// Ranuras de mando de la sala: `COROOM_MAX_GAMEPADS` (1 a 8) o 4.
pub fn max_gamepads() -> u8 {
    std::env::var("COROOM_MAX_GAMEPADS")
        .ok()
        .and_then(|value| value.trim().parse::<u8>().ok())
        .map_or(coroom_proto::input::DEFAULT_GAMEPAD_SLOTS, |slots| {
            slots.clamp(1, coroom_proto::input::MAX_GAMEPAD_SLOTS)
        })
}
