//! Teclas y mandos del invitado de Android, con los números que da la aplicación por JNI.
//!
//! No depende de Android (se compila y se prueba en todas las plataformas): la aplicación en
//! Kotlin pasa los campos de `KeyEvent`, `MotionEvent` e `InputDevice` como números y aquí salen
//! los [`InputEvent`] de siempre.
//!
//! - **Teclas** ([`evdev_from_android`]): un teclado físico (USB o Bluetooth) da en
//!   `KeyEvent.getScanCode()` el código **evdev** de Linux de la tecla física, que es el formato
//!   del cable, y se usa tal cual. Sin él (el teclado en pantalla, un IME), el `KEYCODE_*` de
//!   Android se pasa a evdev con la tabla [`KEYCODES`], la de una tecla de un teclado de PC (los
//!   caracteres que en un PC necesitan Mayúsculas, como `@`, no tienen tecla propia y no están).
//!   Qué teclas no se reenvían (el volumen y «atrás» del propio teléfono) lo decide la aplicación.
//! - **Mandos** ([`AndroidPads`]): Android no deja leer en crudo (HID) los mandos Bluetooth, así
//!   que van **convertidos**, con el vocabulario de Xbox que ya usa Android
//!   (`KEYCODE_BUTTON_*`, `AXIS_*`), los mismos valores que el camino de `gilrs` del escritorio
//!   ([`stick_value`], [`trigger_value`], [`AxisFilter`]) y la misma elección de qué mando va a la
//!   ranura ([`PadChoice`]): se anuncian por [`PadLink`] con `GamepadConnected`/`Disconnected` y
//!   `PadData::Device` con [`GamepadTransport::Converted`].
//! - **Vibración**: lo que pide el anfitrión para la ranura ([`PadLink::rumble`]) llega a
//!   [`AndroidMotors`], que implementa la aplicación con el `VibratorManager` del mando
//!   (`InputDevice.getVibratorManager()`), con el aviso de una vez de [`PadRumble`] si no vibra.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_proto::input::{GamepadDevice, GamepadTransport, PadData};
use tracing::debug;

use crate::gamepad::{
    AxisFilter, ConnectedPad, DECLARE_RETRY, Motors, PadChoice, PadKey, PadKind, PadLink,
    PadRumble, pick_index, stick_value, trigger_value,
};
use crate::keymap::evdev;
use crate::{GamepadAxis, GamepadButton, GamepadKind, InputEvent, Rumble};

/// Constantes `KEYCODE_*` de `android.view.KeyEvent` que se usan fuera de [`KEYCODES`].
pub mod keycode {
    /// `KEYCODE_BACK` (algunos mandos dan así el botón Back / View).
    pub const KEYCODE_BACK: i32 = 4;
    /// `KEYCODE_DPAD_UP`.
    pub const KEYCODE_DPAD_UP: i32 = 19;
    /// `KEYCODE_DPAD_DOWN`.
    pub const KEYCODE_DPAD_DOWN: i32 = 20;
    /// `KEYCODE_DPAD_LEFT`.
    pub const KEYCODE_DPAD_LEFT: i32 = 21;
    /// `KEYCODE_DPAD_RIGHT`.
    pub const KEYCODE_DPAD_RIGHT: i32 = 22;
    /// `KEYCODE_MENU` (algunos mandos dan así el botón Start / Menu).
    pub const KEYCODE_MENU: i32 = 82;
    /// `KEYCODE_BUTTON_A`.
    pub const KEYCODE_BUTTON_A: i32 = 96;
    /// `KEYCODE_BUTTON_B`.
    pub const KEYCODE_BUTTON_B: i32 = 97;
    /// `KEYCODE_BUTTON_X`.
    pub const KEYCODE_BUTTON_X: i32 = 99;
    /// `KEYCODE_BUTTON_Y`.
    pub const KEYCODE_BUTTON_Y: i32 = 100;
    /// `KEYCODE_BUTTON_L1`.
    pub const KEYCODE_BUTTON_L1: i32 = 102;
    /// `KEYCODE_BUTTON_R1`.
    pub const KEYCODE_BUTTON_R1: i32 = 103;
    /// `KEYCODE_BUTTON_L2`.
    pub const KEYCODE_BUTTON_L2: i32 = 104;
    /// `KEYCODE_BUTTON_R2`.
    pub const KEYCODE_BUTTON_R2: i32 = 105;
    /// `KEYCODE_BUTTON_THUMBL`.
    pub const KEYCODE_BUTTON_THUMBL: i32 = 106;
    /// `KEYCODE_BUTTON_THUMBR`.
    pub const KEYCODE_BUTTON_THUMBR: i32 = 107;
    /// `KEYCODE_BUTTON_START`.
    pub const KEYCODE_BUTTON_START: i32 = 108;
    /// `KEYCODE_BUTTON_SELECT`.
    pub const KEYCODE_BUTTON_SELECT: i32 = 109;
    /// `KEYCODE_BUTTON_MODE`.
    pub const KEYCODE_BUTTON_MODE: i32 = 110;
}

/// Constantes `AXIS_*` de `android.view.MotionEvent` de un mando.
pub mod axis {
    /// `AXIS_X`: stick izquierdo, horizontal (`-1` izquierda).
    pub const AXIS_X: i32 = 0;
    /// `AXIS_Y`: stick izquierdo, vertical (`-1` arriba).
    pub const AXIS_Y: i32 = 1;
    /// `AXIS_Z`: stick derecho, horizontal.
    pub const AXIS_Z: i32 = 11;
    /// `AXIS_RZ`: stick derecho, vertical (`-1` arriba).
    pub const AXIS_RZ: i32 = 14;
    /// `AXIS_HAT_X`: cruceta como eje, horizontal (`-1` izquierda).
    pub const AXIS_HAT_X: i32 = 15;
    /// `AXIS_HAT_Y`: cruceta como eje, vertical (`-1` arriba).
    pub const AXIS_HAT_Y: i32 = 16;
    /// `AXIS_LTRIGGER`: gatillo izquierdo (`0..=1`).
    pub const AXIS_LTRIGGER: i32 = 17;
    /// `AXIS_RTRIGGER`: gatillo derecho.
    pub const AXIS_RTRIGGER: i32 = 18;
    /// `AXIS_GAS`: gatillo derecho en algunos mandos (y a la vez que `AXIS_RTRIGGER` en otros).
    pub const AXIS_GAS: i32 = 22;
    /// `AXIS_BRAKE`: gatillo izquierdo en algunos mandos (y a la vez que `AXIS_LTRIGGER` en otros).
    pub const AXIS_BRAKE: i32 = 23;
}

/// `KEYCODE_*` de Android → código evdev, para las teclas sin *scancode* (teclado en pantalla,
/// IME). De las constantes de `android.view.KeyEvent` y `linux/input-event-codes.h`, como el
/// `Generic.kl` de Android al revés.
pub const KEYCODES: &[(i32, u16)] = &[
    // Cifras: `KEYCODE_0`..`KEYCODE_9` (7..16) → `KEY_0` (11) y `KEY_1`..`KEY_9` (2..10).
    (7, 11),
    (8, 2),
    (9, 3),
    (10, 4),
    (11, 5),
    (12, 6),
    (13, 7),
    (14, 8),
    (15, 9),
    (16, 10),
    // Letras: `KEYCODE_A`..`KEYCODE_Z` (29..54) → su tecla de la fila QWERTY.
    (29, 30),
    (30, 48),
    (31, 46),
    (32, 32),
    (33, 18),
    (34, 33),
    (35, 34),
    (36, 35),
    (37, 23),
    (38, 36),
    (39, 37),
    (40, 38),
    (41, 50),
    (42, 49),
    (43, 24),
    (44, 25),
    (45, 16),
    (46, 19),
    (47, 31),
    (48, 20),
    (49, 22),
    (50, 47),
    (51, 17),
    (52, 45),
    (53, 21),
    (54, 44),
    // Puntuación de un teclado de PC (EE. UU.).
    (55, 51), // COMMA → KEY_COMMA
    (56, 52), // PERIOD → KEY_DOT
    (68, 41), // GRAVE → KEY_GRAVE
    (69, 12), // MINUS → KEY_MINUS
    (70, 13), // EQUALS → KEY_EQUAL
    (71, 26), // LEFT_BRACKET → KEY_LEFTBRACE
    (72, 27), // RIGHT_BRACKET → KEY_RIGHTBRACE
    (73, 43), // BACKSLASH → KEY_BACKSLASH
    (74, 39), // SEMICOLON → KEY_SEMICOLON
    (75, 40), // APOSTROPHE → KEY_APOSTROPHE
    (76, 53), // SLASH → KEY_SLASH
    // Modificadores.
    (57, 56),   // ALT_LEFT → KEY_LEFTALT
    (58, 100),  // ALT_RIGHT → KEY_RIGHTALT
    (59, 42),   // SHIFT_LEFT → KEY_LEFTSHIFT
    (60, 54),   // SHIFT_RIGHT → KEY_RIGHTSHIFT
    (113, 29),  // CTRL_LEFT → KEY_LEFTCTRL
    (114, 97),  // CTRL_RIGHT → KEY_RIGHTCTRL
    (115, 58),  // CAPS_LOCK → KEY_CAPSLOCK
    (116, 70),  // SCROLL_LOCK → KEY_SCROLLLOCK
    (117, 125), // META_LEFT → KEY_LEFTMETA
    (118, 126), // META_RIGHT → KEY_RIGHTMETA
    (143, 69),  // NUM_LOCK → KEY_NUMLOCK
    // Edición y navegación.
    (61, 15),   // TAB → KEY_TAB
    (62, 57),   // SPACE → KEY_SPACE
    (66, 28),   // ENTER → KEY_ENTER
    (67, 14),   // DEL (borrar hacia atrás) → KEY_BACKSPACE
    (111, 1),   // ESCAPE → KEY_ESC
    (112, 111), // FORWARD_DEL → KEY_DELETE
    (124, 110), // INSERT → KEY_INSERT
    (122, 102), // MOVE_HOME → KEY_HOME
    (123, 107), // MOVE_END → KEY_END
    (92, 104),  // PAGE_UP → KEY_PAGEUP
    (93, 109),  // PAGE_DOWN → KEY_PAGEDOWN
    (19, 103),  // DPAD_UP → KEY_UP
    (20, 108),  // DPAD_DOWN → KEY_DOWN
    (21, 105),  // DPAD_LEFT → KEY_LEFT
    (22, 106),  // DPAD_RIGHT → KEY_RIGHT
    (120, 99),  // SYSRQ → KEY_SYSRQ (Impr Pant)
    (121, 119), // BREAK → KEY_PAUSE
    (82, 127),  // MENU → KEY_COMPOSE (la tecla de menú contextual)
    // F1..F12: `KEYCODE_F1`..`KEYCODE_F12` (131..142).
    (131, 59),
    (132, 60),
    (133, 61),
    (134, 62),
    (135, 63),
    (136, 64),
    (137, 65),
    (138, 66),
    (139, 67),
    (140, 68),
    (141, 87),
    (142, 88),
    // Bloque numérico: `KEYCODE_NUMPAD_*` (144..163).
    (144, 82),  // NUMPAD_0 → KEY_KP0
    (145, 79),  // NUMPAD_1 → KEY_KP1
    (146, 80),  // NUMPAD_2 → KEY_KP2
    (147, 81),  // NUMPAD_3 → KEY_KP3
    (148, 75),  // NUMPAD_4 → KEY_KP4
    (149, 76),  // NUMPAD_5 → KEY_KP5
    (150, 77),  // NUMPAD_6 → KEY_KP6
    (151, 71),  // NUMPAD_7 → KEY_KP7
    (152, 72),  // NUMPAD_8 → KEY_KP8
    (153, 73),  // NUMPAD_9 → KEY_KP9
    (154, 98),  // NUMPAD_DIVIDE → KEY_KPSLASH
    (155, 55),  // NUMPAD_MULTIPLY → KEY_KPASTERISK
    (156, 74),  // NUMPAD_SUBTRACT → KEY_KPMINUS
    (157, 78),  // NUMPAD_ADD → KEY_KPPLUS
    (158, 83),  // NUMPAD_DOT → KEY_KPDOT
    (159, 121), // NUMPAD_COMMA → KEY_KPCOMMA
    (160, 96),  // NUMPAD_ENTER → KEY_KPENTER
    (161, 117), // NUMPAD_EQUALS → KEY_KPEQUAL
    (162, 179), // NUMPAD_LEFT_PAREN → KEY_KPLEFTPAREN
    (163, 180), // NUMPAD_RIGHT_PAREN → KEY_KPRIGHTPAREN
    // Multimedia y aplicaciones.
    (24, 115),  // VOLUME_UP → KEY_VOLUMEUP
    (25, 114),  // VOLUME_DOWN → KEY_VOLUMEDOWN
    (164, 113), // VOLUME_MUTE → KEY_MUTE
    (85, 164),  // MEDIA_PLAY_PAUSE → KEY_PLAYPAUSE
    (86, 166),  // MEDIA_STOP → KEY_STOPCD
    (87, 163),  // MEDIA_NEXT → KEY_NEXTSONG
    (88, 165),  // MEDIA_PREVIOUS → KEY_PREVIOUSSONG
    (89, 168),  // MEDIA_REWIND → KEY_REWIND
    (90, 208),  // MEDIA_FAST_FORWARD → KEY_FASTFORWARD
    (126, 200), // MEDIA_PLAY → KEY_PLAYCD
    (127, 201), // MEDIA_PAUSE → KEY_PAUSECD
    (129, 161), // MEDIA_EJECT → KEY_EJECTCD
    (130, 167), // MEDIA_RECORD → KEY_RECORD
    (64, 150),  // EXPLORER → KEY_WWW
    (65, 155),  // ENVELOPE → KEY_MAIL
    (84, 217),  // SEARCH → KEY_SEARCH
    (125, 159), // FORWARD → KEY_FORWARD
    (174, 156), // BOOKMARK → KEY_BOOKMARKS
    (210, 140), // CALCULATOR → KEY_CALC
    (223, 142), // SLEEP → KEY_SLEEP
    (224, 143), // WAKEUP → KEY_WAKEUP
    (220, 224), // BRIGHTNESS_DOWN → KEY_BRIGHTNESSDOWN
    (221, 225), // BRIGHTNESS_UP → KEY_BRIGHTNESSUP
    (259, 138), // HELP → KEY_HELP
    (277, 137), // CUT → KEY_CUT
    (278, 133), // COPY → KEY_COPY
    (279, 135), // PASTE → KEY_PASTE
    (285, 173), // REFRESH → KEY_REFRESH
    // Teclados japoneses.
    (211, 85),  // ZENKAKU_HANKAKU → KEY_ZENKAKUHANKAKU
    (213, 94),  // MUHENKAN → KEY_MUHENKAN
    (214, 92),  // HENKAN → KEY_HENKAN
    (215, 93),  // KATAKANA_HIRAGANA → KEY_KATAKANAHIRAGANA
    (216, 124), // YEN → KEY_YEN
    (217, 89),  // RO → KEY_RO
];

/// Código evdev de un `KeyEvent` de teclado: su *scancode* (`getScanCode()`) si es el de una
/// tecla de teclado de Linux (un teclado físico), o si no su `getKeyCode()` por [`KEYCODES`].
/// `None` si no es una tecla que vaya al anfitrión (los botones de mando van a [`AndroidPads`]).
pub fn evdev_from_android(keycode: i32, scancode: i32) -> Option<u16> {
    if let Ok(scancode) = u16::try_from(scancode)
        && scancode > 0
        && usize::from(scancode) < evdev::KEY_LIMIT
    {
        return Some(scancode);
    }
    KEYCODES
        .iter()
        .find(|(android, _)| *android == keycode)
        .map(|(_, code)| *code)
}

/// Botón de mando de un `KEYCODE_*` de un mando (los gatillos digitales y la cruceta, aparte).
pub fn pad_button(keycode: i32) -> Option<GamepadButton> {
    use keycode::*;
    Some(match keycode {
        KEYCODE_BUTTON_A => GamepadButton::A,
        KEYCODE_BUTTON_B => GamepadButton::B,
        KEYCODE_BUTTON_X => GamepadButton::X,
        KEYCODE_BUTTON_Y => GamepadButton::Y,
        KEYCODE_BUTTON_L1 => GamepadButton::LeftBumper,
        KEYCODE_BUTTON_R1 => GamepadButton::RightBumper,
        KEYCODE_BUTTON_SELECT | KEYCODE_BACK => GamepadButton::Back,
        KEYCODE_BUTTON_START | KEYCODE_MENU => GamepadButton::Start,
        KEYCODE_BUTTON_MODE => GamepadButton::Guide,
        KEYCODE_BUTTON_THUMBL => GamepadButton::LeftStick,
        KEYCODE_BUTTON_THUMBR => GamepadButton::RightStick,
        _ => return None,
    })
}

/// Los ejes de un `MotionEvent` de un mando: `getAxisValue(AXIS_*)` de cada uno (0 si el mando no
/// lo tiene, como da Android).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PadAxes {
    /// `AXIS_X`.
    pub x: f32,
    /// `AXIS_Y`.
    pub y: f32,
    /// `AXIS_Z`.
    pub z: f32,
    /// `AXIS_RZ`.
    pub rz: f32,
    /// `AXIS_LTRIGGER`.
    pub ltrigger: f32,
    /// `AXIS_RTRIGGER`.
    pub rtrigger: f32,
    /// `AXIS_BRAKE`.
    pub brake: f32,
    /// `AXIS_GAS`.
    pub gas: f32,
    /// `AXIS_HAT_X`.
    pub hat_x: f32,
    /// `AXIS_HAT_Y`.
    pub hat_y: f32,
}

impl PadAxes {
    /// Los ejes que lee [`AndroidPads::on_motion`], para pedirlos a `MotionEvent` uno a uno.
    pub const AXES: [i32; 10] = [
        axis::AXIS_X,
        axis::AXIS_Y,
        axis::AXIS_Z,
        axis::AXIS_RZ,
        axis::AXIS_LTRIGGER,
        axis::AXIS_RTRIGGER,
        axis::AXIS_BRAKE,
        axis::AXIS_GAS,
        axis::AXIS_HAT_X,
        axis::AXIS_HAT_Y,
    ];

    /// Pone el eje `axis` (un `AXIS_*`); los demás se ignoran.
    pub fn set(&mut self, axis: i32, value: f32) {
        let slot = match axis {
            axis::AXIS_X => &mut self.x,
            axis::AXIS_Y => &mut self.y,
            axis::AXIS_Z => &mut self.z,
            axis::AXIS_RZ => &mut self.rz,
            axis::AXIS_LTRIGGER => &mut self.ltrigger,
            axis::AXIS_RTRIGGER => &mut self.rtrigger,
            axis::AXIS_BRAKE => &mut self.brake,
            axis::AXIS_GAS => &mut self.gas,
            axis::AXIS_HAT_X => &mut self.hat_x,
            axis::AXIS_HAT_Y => &mut self.hat_y,
            _ => return,
        };
        *slot = value;
    }

    /// Con los valores de [`Self::AXES`] en ese orden (los que falten, 0).
    pub fn from_values(values: &[f32]) -> Self {
        let mut axes = Self::default();
        for (axis, value) in Self::AXES.iter().zip(values) {
            axes.set(*axis, *value);
        }
        axes
    }
}

/// El `Instant` de un evento de Android: `event_time_ms` es su `getEventTime()` y `uptime_ms` el
/// `SystemClock.uptimeMillis()` de ahora (los dos en el mismo reloj): ahora menos lo que ha pasado
/// desde entonces, para que la etapa captura → envío se mida desde que el sistema leyó el mando.
pub fn event_instant(event_time_ms: u64, uptime_ms: u64) -> Instant {
    let now = Instant::now();
    let age = Duration::from_millis(uptime_ms.saturating_sub(event_time_ms));
    now.checked_sub(age).unwrap_or(now)
}

/// Por qué un mando de Android va convertido (para el anfitrión, en `PadData::Device`).
pub const CONVERTED_REASON: &str =
    "Android no deja leer en crudo (HID) los mandos Bluetooth en el equipo del invitado";

/// Lo mismo, corto, para la lista de mandos de la interfaz.
pub const CONVERTED_KIND: &str = "Android no los da en crudo";

/// Los motores de los mandos del móvil, que mueve la aplicación (`InputDevice.getVibratorManager()`
/// del mando, con un efecto por motor). La puerta JNI lo implementa llamando a Kotlin.
pub trait AndroidMotors: Send {
    /// Pone los motores del mando `device_id` (`InputDevice.getId()`), el de la ranura `slot`, en
    /// `rumble` (vale hasta el siguiente; [`Rumble::OFF`] los para). Un error dice por qué no
    /// vibra (sin vibrador, por ejemplo), y se avisa una vez.
    fn set(&mut self, device_id: i32, slot: u8, rumble: Rumble) -> Result<(), String>;
}

/// Un mando de Android visto como [`Motors`] de [`PadRumble`].
struct DeviceMotors<'a> {
    motors: &'a mut dyn AndroidMotors,
    device_id: i32,
    slot: u8,
}

impl Motors for DeviceMotors<'_> {
    fn set(&mut self, rumble: Rumble) -> Result<(), String> {
        self.motors.set(self.device_id, self.slot, rumble)
    }
}

/// Qué mirar si un mando de Android no vibra.
const RUMBLE_HELP: &str = "Android no da a la aplicación los motores de ese mando";

/// La cruceta: arriba, abajo, izquierda y derecha.
const DPAD: [GamepadButton; 4] = [
    GamepadButton::DPadUp,
    GamepadButton::DPadDown,
    GamepadButton::DPadLeft,
    GamepadButton::DPadRight,
];

/// Lo que se sabe de un mando conectado.
#[derive(Debug)]
struct Device {
    key: PadKey,
    /// Botones pulsados (sin la cruceta), en el orden de [`GamepadButton::ALL`].
    buttons: [bool; GamepadButton::ALL.len()],
    /// La cruceta por teclas y por los ejes `HAT`: pulsada si lo está por cualquiera.
    dpad_keys: [bool; 4],
    dpad_hat: [bool; 4],
    /// Los gatillos digitales (`KEYCODE_BUTTON_L2`/`R2`) y si el mando los da también como eje
    /// (entonces las teclas se ignoran: el eje ya dice cuánto).
    trigger_keys: [bool; 2],
    analog_triggers: [bool; 2],
    /// El último valor de cada gatillo por eje.
    triggers: [f32; 2],
}

impl Device {
    fn new(key: PadKey) -> Self {
        Self {
            key,
            buttons: [false; GamepadButton::ALL.len()],
            dpad_keys: [false; 4],
            dpad_hat: [false; 4],
            trigger_keys: [false; 2],
            analog_triggers: [false; 2],
            triggers: [0.0; 2],
        }
    }

    fn dpad(&self, index: usize) -> bool {
        self.dpad_keys[index] || self.dpad_hat[index]
    }

    /// El valor de un gatillo (0 izquierdo, 1 derecho), de `0` a `1`.
    fn trigger(&self, index: usize) -> f32 {
        if self.analog_triggers[index] {
            self.triggers[index]
        } else if self.trigger_keys[index] {
            1.0
        } else {
            0.0
        }
    }
}

/// Los mandos del móvil, por su `InputDevice.getId()`: cuál va a la ranura y qué manda.
///
/// Como el hilo de `gilrs` del escritorio ([`crate::gamepad::GamepadCapture`]), un solo mando va
/// a la ranura que asigna el anfitrión ([`Self::set_slot`]): el que elija el invitado o, en
/// automático, el último conectado; al conectarse, desconectarse o cambiar de ranura se anuncia
/// con `GamepadConnected`/`GamepadDisconnected` y `PadData::Device` (convertido). Aquí no hay
/// hilo: la aplicación llama desde el suyo (con un cerrojo si son varios) al conectar y
/// desconectar mandos (`InputManager.InputDeviceListener`), con cada `KeyEvent` y `MotionEvent`
/// de un mando, y a [`Self::poll`] cada [`crate::gamepad::IDLE_POLL`] para la vibración. Los ejes
/// salen con cada `MotionEvent` (Android ya los agrupa), solo si cambian ([`AxisFilter`]).
pub struct AndroidPads {
    link: Arc<dyn PadLink>,
    motors: Box<dyn AndroidMotors>,
    choice: PadChoice,
    slot: Option<u8>,
    /// En orden de conexión.
    devices: Vec<(i32, Device)>,
    /// Lo que se anunció al anfitrión: ranura y mando.
    announced: Option<(u8, i32)>,
    axes: AxisFilter,
    rumble: Option<PadRumble>,
    /// La declaración del mando de la ranura y cuándo salió, para repetirla mientras el
    /// anfitrión no la conteste ([`PadLink::answered`]).
    declared: Option<(PadData, Instant)>,
}

impl std::fmt::Debug for AndroidPads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AndroidPads")
            .field("choice", &self.choice)
            .field("slot", &self.slot)
            .field("devices", &self.devices)
            .field("announced", &self.announced)
            .finish_non_exhaustive()
    }
}

impl AndroidPads {
    /// Sin mandos ni ranura; lo convertido y la vibración van por `link` y `motors`.
    pub fn new(link: Arc<dyn PadLink>, choice: PadChoice, motors: Box<dyn AndroidMotors>) -> Self {
        Self {
            link,
            motors,
            choice,
            slot: None,
            devices: Vec::new(),
            announced: None,
            axes: AxisFilter::default(),
            rumble: None,
            declared: None,
        }
    }

    /// Ranura asignada por el anfitrión (`None`: sin mando).
    pub fn set_slot(&mut self, slot: Option<u8>) {
        self.slot = slot.filter(|slot| *slot != 0);
        self.reassign();
    }

    /// Qué mando mandar.
    pub fn set_choice(&mut self, choice: PadChoice) {
        self.choice = choice;
        self.reassign();
    }

    /// Mandos conectados ahora, para la interfaz.
    pub fn pads(&self) -> Vec<ConnectedPad> {
        let active = self.announced.map(|(_, id)| id);
        self.devices
            .iter()
            .map(|(id, device)| ConnectedPad {
                key: device.key.clone(),
                kind: PadKind::Converted(CONVERTED_KIND.to_owned()),
                active: Some(*id) == active,
            })
            .collect()
    }

    /// El `InputDevice.getId()` del mando que va a la ranura, y la ranura.
    pub fn active(&self) -> Option<(u8, i32)> {
        self.announced
    }

    /// Un mando conectado (`onInputDeviceAdded`, o los que había al empezar), con su
    /// `getVendorId()`, `getProductId()` y `getName()`. Si ya estaba (`onInputDeviceChanged`),
    /// solo se actualiza lo que se sabe de él.
    pub fn device_added(&mut self, device_id: i32, key: PadKey) {
        if let Some((_, device)) = self.devices.iter_mut().find(|(id, _)| *id == device_id) {
            device.key = key;
        } else {
            debug!(name = %key.name, "mando de Android conectado");
            self.devices.push((device_id, Device::new(key)));
        }
        // Con otro nombre puede dejar de ser (o pasar a ser) el elegido.
        self.reassign();
    }

    /// Un mando desconectado (`onInputDeviceRemoved`).
    pub fn device_removed(&mut self, device_id: i32) {
        let before = self.devices.len();
        self.devices.retain(|(id, _)| *id != device_id);
        if self.devices.len() != before {
            self.reassign();
        }
    }

    /// Un `KeyEvent` de un mando (`ACTION_DOWN` es `pressed`), ocurrido en `at`
    /// ([`event_instant`]). `true` si es un botón de un mando conectado: la aplicación no debe
    /// pasarlo a Android (`KEYCODE_BACK` saldría de la pantalla) ni al teclado.
    pub fn on_key(&mut self, device_id: i32, keycode: i32, pressed: bool, at: Instant) -> bool {
        let Some(index) = self.device_index(device_id) else {
            return false;
        };
        let device = &mut self.devices[index].1;
        let dpad = match keycode {
            keycode::KEYCODE_DPAD_UP => Some(0),
            keycode::KEYCODE_DPAD_DOWN => Some(1),
            keycode::KEYCODE_DPAD_LEFT => Some(2),
            keycode::KEYCODE_DPAD_RIGHT => Some(3),
            _ => None,
        };
        if let Some(dpad) = dpad {
            let before = device.dpad(dpad);
            device.dpad_keys[dpad] = pressed;
            let after = device.dpad(dpad);
            if before != after {
                self.button(device_id, DPAD[dpad], after, at);
            }
            return true;
        }
        let trigger = match keycode {
            keycode::KEYCODE_BUTTON_L2 => Some(0),
            keycode::KEYCODE_BUTTON_R2 => Some(1),
            _ => None,
        };
        if let Some(trigger) = trigger {
            device.trigger_keys[trigger] = pressed;
            if !device.analog_triggers[trigger] {
                let value = device.trigger(trigger);
                self.trigger_axes(device_id, at, [(trigger, value)]);
            }
            return true;
        }
        let Some(button) = pad_button(keycode) else {
            return false;
        };
        let held = &mut device.buttons[button_index(button)];
        if *held != pressed {
            *held = pressed;
            self.button(device_id, button, pressed, at);
        }
        true
    }

    /// Un `MotionEvent` de un mando (`SOURCE_JOYSTICK`) con sus ejes, ocurrido en `at`. `true` si
    /// es de un mando conectado.
    pub fn on_motion(&mut self, device_id: i32, axes: &PadAxes, at: Instant) -> bool {
        let Some(index) = self.device_index(device_id) else {
            return false;
        };
        let device = &mut self.devices[index].1;
        // La cruceta como eje: pulsada pasada la mitad (Android da -1, 0 o 1).
        let hat = [
            axes.hat_y < -0.5,
            axes.hat_y > 0.5,
            axes.hat_x < -0.5,
            axes.hat_x > 0.5,
        ];
        let mut changed = [None; 4];
        for (index, pressed) in hat.into_iter().enumerate() {
            let before = device.dpad(index);
            device.dpad_hat[index] = pressed;
            let after = device.dpad(index);
            if before != after {
                changed[index] = Some(after);
            }
        }
        // Los gatillos: unos mandos los dan en `LTRIGGER`/`RTRIGGER`, otros en `BRAKE`/`GAS` y
        // otros en los dos a la vez; vale el mayor (y un solo eje por gatillo).
        let analog = [
            finite(axes.ltrigger).max(finite(axes.brake)),
            finite(axes.rtrigger).max(finite(axes.gas)),
        ];
        for (index, value) in analog.into_iter().enumerate() {
            if value > 0.0 {
                device.analog_triggers[index] = true;
            }
            device.triggers[index] = value;
        }
        let triggers = [(0, device.trigger(0)), (1, device.trigger(1))];
        for (index, pressed) in changed.into_iter().enumerate() {
            if let Some(pressed) = pressed {
                self.button(device_id, DPAD[index], pressed, at);
            }
        }
        if self.slot_of(device_id).is_none() {
            return true;
        }
        // El vertical de Android es positivo hacia abajo; el del cable, hacia arriba.
        let sticks = [
            (GamepadAxis::LeftX, stick_value(axes.x)),
            (GamepadAxis::LeftY, stick_value(-axes.y)),
            (GamepadAxis::RightX, stick_value(axes.z)),
            (GamepadAxis::RightY, stick_value(-axes.rz)),
        ];
        for (axis, value) in sticks {
            self.axes.set_at(axis, value, at);
        }
        self.trigger_axes(device_id, at, triggers);
        true
    }

    /// La vibración que pide el anfitrión para la ranura, al mando que va en ella (si cambió).
    pub fn poll(&mut self) {
        // La declaración, otra vez, mientras el anfitrión no la conteste.
        if let (Some((slot, _)), Some((declared, at))) = (self.announced, self.declared.as_mut())
            && at.elapsed() >= DECLARE_RETRY
            && !self.link.answered(slot)
        {
            *at = Instant::now();
            self.link.pad(declared.clone());
        }
        let (Some((slot, device_id)), Some(state)) = (self.announced, self.rumble.as_mut()) else {
            return;
        };
        let wanted = self.link.rumble(slot);
        state.deliver(
            &mut DeviceMotors {
                motors: self.motors.as_mut(),
                device_id,
                slot,
            },
            wanted,
        );
    }

    fn device_index(&self, device_id: i32) -> Option<usize> {
        self.devices.iter().position(|(id, _)| *id == device_id)
    }

    /// La ranura de `device_id` si es el que va en ella.
    fn slot_of(&self, device_id: i32) -> Option<u8> {
        self.announced
            .filter(|(_, id)| *id == device_id)
            .map(|(slot, _)| slot)
    }

    fn button(&self, device_id: i32, button: GamepadButton, pressed: bool, at: Instant) {
        if let Some(slot) = self.slot_of(device_id) {
            self.link.event_at(
                InputEvent::GamepadButton {
                    slot,
                    button,
                    pressed,
                },
                at,
            );
        }
    }

    /// Anota los gatillos y manda los ejes que cambiaron (con los sticks pendientes).
    fn trigger_axes(
        &mut self,
        device_id: i32,
        at: Instant,
        triggers: impl IntoIterator<Item = (usize, f32)>,
    ) {
        let Some(slot) = self.slot_of(device_id) else {
            return;
        };
        for (index, value) in triggers {
            let axis = [GamepadAxis::LeftTrigger, GamepadAxis::RightTrigger][index];
            self.axes.set_at(axis, trigger_value(value), at);
        }
        let link = &self.link;
        self.axes.drain_changed(|axis, value, at| {
            link.event_at(InputEvent::GamepadAxis { slot, axis, value }, at);
        });
    }

    /// Qué mando va a qué ranura ahora, y lo que cambie se anuncia.
    fn reassign(&mut self) {
        let active = pick_index(&self.choice, self.devices.iter().map(|(_, d)| &d.key))
            .map(|index| &self.devices[index]);
        let wanted = self.slot.zip(active.map(|(id, _)| *id));
        let key = active.map(|(_, device)| device.key.clone());
        if wanted == self.announced {
            return;
        }
        if let Some((old, old_id)) = self.announced.take() {
            self.link
                .event(InputEvent::GamepadDisconnected { slot: old });
            // El mando deja la ranura: sin motores en marcha.
            if let Some(mut state) = self.rumble.take() {
                state.stop(&mut DeviceMotors {
                    motors: self.motors.as_mut(),
                    device_id: old_id,
                    slot: old,
                });
            }
        }
        self.axes.reset();
        self.declared = None;
        if let (Some((slot, _)), Some(key)) = (wanted, key) {
            self.link.event(InputEvent::GamepadConnected {
                slot,
                kind: GamepadKind::Xbox,
            });
            let declared = PadData::Device {
                slot,
                device: GamepadDevice {
                    name: key.name.clone(),
                    vendor: key.vendor,
                    product: key.product,
                    version: 0,
                    // Como los demás convertidos: con un mando convertido el anfitrión no lo usa.
                    bus: 3,
                    transport: GamepadTransport::Converted {
                        reason: CONVERTED_REASON.to_owned(),
                    },
                },
            };
            self.link.pad(declared.clone());
            self.declared = Some((declared, Instant::now()));
            self.rumble = Some(PadRumble::new(&key.name, RUMBLE_HELP));
        }
        self.announced = wanted;
    }
}

impl Drop for AndroidPads {
    fn drop(&mut self) {
        // Al terminar, los motores parados.
        if let (Some((slot, device_id)), Some(state)) = (self.announced, self.rumble.as_mut()) {
            state.stop(&mut DeviceMotors {
                motors: self.motors.as_mut(),
                device_id,
                slot,
            });
        }
    }
}

fn button_index(button: GamepadButton) -> usize {
    GamepadButton::ALL
        .iter()
        .position(|b| *b == button)
        .unwrap_or(0)
}

/// Un valor de eje usable (NaN e infinitos, 0).
fn finite(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn hardware_scancodes_win_over_keycodes() {
        // Un teclado físico: el *scancode* es la tecla física (aquí `KEY_Q` con la distribución
        // francesa, que Android da como `KEYCODE_A`).
        assert_eq!(evdev_from_android(29, 16), Some(16));
        // El teclado en pantalla no tiene *scancode*: vale el `KEYCODE_*`.
        assert_eq!(evdev_from_android(29, 0), Some(30));
        assert_eq!(evdev_from_android(29, -1), Some(30));
        // Un botón de mando (`BTN_SOUTH` = 0x130) no es una tecla de teclado.
        assert_eq!(evdev_from_android(keycode::KEYCODE_BUTTON_A, 0x130), None);
        assert_eq!(evdev_from_android(keycode::KEYCODE_BUTTON_A, 0), None);
        // Lo que no tiene tecla en un PC (`KEYCODE_AT`, `KEYCODE_POWER`), tampoco.
        assert_eq!(evdev_from_android(77, 0), None);
        assert_eq!(evdev_from_android(26, 0), None);
    }

    #[test]
    fn the_keycode_table_has_no_repeats_and_only_keyboard_codes() {
        let keycodes: HashSet<i32> = KEYCODES.iter().map(|(k, _)| *k).collect();
        let codes: HashSet<u16> = KEYCODES.iter().map(|(_, c)| *c).collect();
        assert_eq!(keycodes.len(), KEYCODES.len(), "KEYCODE_* repetidos");
        assert_eq!(codes.len(), KEYCODES.len(), "códigos evdev repetidos");
        for (keycode, code) in KEYCODES {
            assert!(
                *code > 0 && usize::from(*code) < evdev::KEY_LIMIT,
                "{keycode}"
            );
            assert!(pad_button(*keycode).is_none() || *keycode == keycode::KEYCODE_MENU);
        }
    }

    /// Con la tabla de `winit` (que se comprueba contra la del sistema): cada `KEYCODE_*` va a la
    /// tecla de su nombre.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn the_keycode_table_agrees_with_the_desktop_one() {
        use crate::keymap::keycode_from_evdev;
        let name = |keycode: i32| {
            evdev_from_android(keycode, 0)
                .and_then(keycode_from_evdev)
                .map(|key| format!("{key:?}"))
                .unwrap_or_else(|| format!("sin tecla ({keycode})"))
        };
        for (index, letter) in ('A'..='Z').enumerate() {
            assert_eq!(name(29 + index as i32), format!("Key{letter}"));
        }
        for digit in 0..10 {
            assert_eq!(name(7 + digit), format!("Digit{digit}"));
            assert_eq!(name(144 + digit), format!("Numpad{digit}"));
        }
        for f in 1..=12 {
            assert_eq!(name(130 + f), format!("F{f}"));
        }
        let named = [
            (55, "Comma"),
            (56, "Period"),
            (57, "AltLeft"),
            (58, "AltRight"),
            (59, "ShiftLeft"),
            (60, "ShiftRight"),
            (61, "Tab"),
            (62, "Space"),
            (66, "Enter"),
            (67, "Backspace"),
            (68, "Backquote"),
            (69, "Minus"),
            (70, "Equal"),
            (71, "BracketLeft"),
            (72, "BracketRight"),
            (73, "Backslash"),
            (74, "Semicolon"),
            (75, "Quote"),
            (76, "Slash"),
            (82, "ContextMenu"),
            (92, "PageUp"),
            (93, "PageDown"),
            (19, "ArrowUp"),
            (20, "ArrowDown"),
            (21, "ArrowLeft"),
            (22, "ArrowRight"),
            (111, "Escape"),
            (112, "Delete"),
            (113, "ControlLeft"),
            (114, "ControlRight"),
            (115, "CapsLock"),
            (116, "ScrollLock"),
            (117, "SuperLeft"),
            (118, "SuperRight"),
            (120, "PrintScreen"),
            (121, "Pause"),
            (122, "Home"),
            (123, "End"),
            (124, "Insert"),
            (143, "NumLock"),
            (154, "NumpadDivide"),
            (155, "NumpadMultiply"),
            (156, "NumpadSubtract"),
            (157, "NumpadAdd"),
            (158, "NumpadDecimal"),
            (159, "NumpadComma"),
            (160, "NumpadEnter"),
            (161, "NumpadEqual"),
            (24, "AudioVolumeUp"),
            (25, "AudioVolumeDown"),
            (164, "AudioVolumeMute"),
            (85, "MediaPlayPause"),
            (86, "MediaStop"),
            (87, "MediaTrackNext"),
            (88, "MediaTrackPrevious"),
            (65, "LaunchMail"),
            (84, "BrowserSearch"),
            (125, "BrowserForward"),
            (174, "BrowserFavorites"),
            (210, "LaunchApp2"),
            (223, "Sleep"),
            (224, "WakeUp"),
            (285, "BrowserRefresh"),
            (213, "NonConvert"),
            (214, "Convert"),
            (215, "KanaMode"),
            (216, "IntlYen"),
            (217, "IntlRo"),
        ];
        for (keycode, expected) in named {
            assert_eq!(name(keycode), expected, "KEYCODE {keycode}");
        }
        // Las que no tiene la tabla del escritorio (sin *scancode* de PC): el anfitrión de Linux
        // las inyecta igual; el de Windows, no.
        let mut extra: Vec<u16> = KEYCODES
            .iter()
            .map(|(_, code)| *code)
            .filter(|code| keycode_from_evdev(*code).is_none())
            .collect();
        extra.sort_unstable();
        assert_eq!(
            extra,
            vec![
                85, 133, 135, 137, 138, 150, 161, 167, 168, 179, 180, 200, 201, 208, 224, 225
            ]
        );
    }

    #[test]
    fn pad_buttons_use_the_xbox_vocabulary() {
        use keycode::*;
        assert_eq!(pad_button(KEYCODE_BUTTON_A), Some(GamepadButton::A));
        assert_eq!(pad_button(KEYCODE_BUTTON_Y), Some(GamepadButton::Y));
        assert_eq!(
            pad_button(KEYCODE_BUTTON_L1),
            Some(GamepadButton::LeftBumper)
        );
        assert_eq!(pad_button(KEYCODE_BUTTON_SELECT), Some(GamepadButton::Back));
        assert_eq!(pad_button(KEYCODE_BACK), Some(GamepadButton::Back));
        assert_eq!(pad_button(KEYCODE_MENU), Some(GamepadButton::Start));
        assert_eq!(pad_button(KEYCODE_BUTTON_MODE), Some(GamepadButton::Guide));
        assert_eq!(
            pad_button(KEYCODE_BUTTON_THUMBR),
            Some(GamepadButton::RightStick)
        );
        // Los gatillos son ejes y la cruceta va aparte.
        assert_eq!(pad_button(KEYCODE_BUTTON_L2), None);
        assert_eq!(pad_button(KEYCODE_DPAD_UP), None);
    }

    #[test]
    fn axes_are_read_by_their_android_number() {
        let axes = PadAxes::from_values(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]);
        assert_eq!(
            axes,
            PadAxes {
                x: 0.1,
                y: 0.2,
                z: 0.3,
                rz: 0.4,
                ltrigger: 0.5,
                rtrigger: 0.6,
                brake: 0.7,
                gas: 0.8,
                hat_x: 0.9,
                hat_y: 1.0,
            }
        );
        let mut short = PadAxes::from_values(&[1.0]);
        assert_eq!(short.x, 1.0);
        assert_eq!(short.y, 0.0);
        // `AXIS_RX` (12) no se usa.
        short.set(12, 1.0);
        short.set(axis::AXIS_BRAKE, 0.25);
        assert_eq!(short.brake, 0.25);
    }

    #[test]
    fn event_times_become_instants() {
        let at = event_instant(1_000, 1_020);
        // Funcional: un evento de hace 20 ms queda al menos 20 ms antes de ahora.
        let age = Instant::now().saturating_duration_since(at);
        assert!(age >= Duration::from_millis(20), "{age:?}");
        coroom_testkit::timing_assert!(
            age < Duration::from_millis(500),
            "un evento de hace 20 ms sale con {age:?}: el hilo se quedó parado"
        );
        // Uno del futuro (relojes distintos) no se adelanta.
        assert!(event_instant(2_000, 1_000) <= Instant::now());
    }

    /// Un `PadLink` que apunta lo que se le manda y pide la vibración que se le diga.
    #[derive(Default)]
    struct Link {
        events: Mutex<Vec<(InputEvent, Option<Instant>)>>,
        pads: Mutex<Vec<PadData>>,
        rumble: Mutex<Rumble>,
        /// Si el anfitrión todavía no contestó la declaración.
        unanswered: std::sync::atomic::AtomicBool,
    }

    impl Link {
        fn take(&self) -> Vec<InputEvent> {
            std::mem::take(&mut *self.events.lock().unwrap())
                .into_iter()
                .map(|(event, _)| event)
                .collect()
        }

        fn take_pads(&self) -> Vec<PadData> {
            std::mem::take(&mut *self.pads.lock().unwrap())
        }
    }

    impl PadLink for Link {
        fn event(&self, event: InputEvent) {
            self.events.lock().unwrap().push((event, None));
        }
        fn event_at(&self, event: InputEvent, at: Instant) {
            self.events.lock().unwrap().push((event, Some(at)));
        }
        fn pad(&self, data: PadData) {
            self.pads.lock().unwrap().push(data);
        }
        fn raw(&self, _: u8) -> bool {
            false
        }
        fn answered(&self, _: u8) -> bool {
            !self.unanswered.load(std::sync::atomic::Ordering::Acquire)
        }
        fn outputs(&self) -> Vec<(u8, Vec<u8>)> {
            Vec::new()
        }
        fn rumble(&self, _: u8) -> Rumble {
            *self.rumble.lock().unwrap()
        }
    }

    /// Los motores de la aplicación: apuntan lo que se les pone, o fallan como un mando sin
    /// vibrador.
    #[derive(Clone, Default)]
    struct AppMotors {
        set: Arc<Mutex<Vec<(i32, u8, Rumble)>>>,
        unsupported: bool,
    }

    impl AndroidMotors for AppMotors {
        fn set(&mut self, device_id: i32, slot: u8, rumble: Rumble) -> Result<(), String> {
            if self.unsupported {
                return Err("sin vibrador".to_owned());
            }
            self.set.lock().unwrap().push((device_id, slot, rumble));
            Ok(())
        }
    }

    fn key(name: &str) -> PadKey {
        PadKey {
            vendor: 0x1234,
            product: 0x5678,
            name: name.to_owned(),
        }
    }

    fn pads() -> (AndroidPads, Arc<Link>, AppMotors) {
        let link = Arc::new(Link::default());
        let motors = AppMotors::default();
        let pads = AndroidPads::new(
            Arc::clone(&link) as Arc<dyn PadLink>,
            PadChoice::Auto,
            Box::new(motors.clone()),
        );
        (pads, link, motors)
    }

    /// Un mando de ejemplo (`id` 7) en la ranura 2, con lo del anuncio ya leído.
    fn connected() -> (AndroidPads, Arc<Link>, AppMotors) {
        let (mut pads, link, motors) = pads();
        pads.device_added(7, key("Mando de ejemplo"));
        pads.set_slot(Some(2));
        link.take();
        link.take_pads();
        (pads, link, motors)
    }

    fn pressed(slot: u8, button: GamepadButton, pressed: bool) -> InputEvent {
        InputEvent::GamepadButton {
            slot,
            button,
            pressed,
        }
    }

    fn axis(slot: u8, axis: GamepadAxis, value: i16) -> InputEvent {
        InputEvent::GamepadAxis { slot, axis, value }
    }

    /// Mientras el anfitrión no contesta la declaración, se repite; contestada, no.
    #[test]
    fn the_declaration_is_repeated_until_the_host_answers() {
        use std::sync::atomic::Ordering;
        let (mut pads, link, _) = pads();
        link.unanswered.store(true, Ordering::Release);
        pads.device_added(7, key("Mando de ejemplo"));
        pads.set_slot(Some(2));
        let first = link.take_pads();
        assert_eq!(first.len(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        let repeated = loop {
            pads.poll();
            let sent = link.take_pads();
            if !sent.is_empty() || Instant::now() >= deadline {
                break sent;
            }
            std::thread::yield_now();
        };
        assert_eq!(repeated, first, "se repite la misma declaración");

        link.unanswered.store(false, Ordering::Release);
        let quiet = Instant::now() + 2 * DECLARE_RETRY;
        while Instant::now() < quiet {
            pads.poll();
            std::thread::yield_now();
        }
        assert!(link.take_pads().is_empty(), "contestada, no se repite");
    }

    #[test]
    fn a_pad_is_announced_converted_when_it_gets_a_slot() {
        let (mut pads, link, _) = pads();
        pads.device_added(7, key("Mando de ejemplo"));
        // Sin ranura no se manda nada.
        assert!(link.take().is_empty());
        assert!(link.take_pads().is_empty());
        assert_eq!(
            pads.pads(),
            vec![ConnectedPad {
                key: key("Mando de ejemplo"),
                kind: PadKind::Converted(CONVERTED_KIND.to_owned()),
                active: false,
            }]
        );
        pads.set_slot(Some(2));
        assert_eq!(
            link.take(),
            vec![InputEvent::GamepadConnected {
                slot: 2,
                kind: GamepadKind::Xbox
            }]
        );
        assert_eq!(
            link.take_pads(),
            vec![PadData::Device {
                slot: 2,
                device: GamepadDevice {
                    name: "Mando de ejemplo".to_owned(),
                    vendor: 0x1234,
                    product: 0x5678,
                    version: 0,
                    bus: 3,
                    transport: GamepadTransport::Converted {
                        reason: CONVERTED_REASON.to_owned()
                    },
                },
            }]
        );
        assert!(pads.pads()[0].active);
        assert_eq!(pads.active(), Some((2, 7)));
        // Lo mismo otra vez no anuncia nada; `onInputDeviceChanged` tampoco.
        pads.set_slot(Some(2));
        pads.device_added(7, key("Mando de ejemplo"));
        assert!(link.take().is_empty());
        // Otra ranura: se va de una y llega a la otra; sin ranura, se va.
        pads.set_slot(Some(3));
        assert_eq!(
            link.take(),
            vec![
                InputEvent::GamepadDisconnected { slot: 2 },
                InputEvent::GamepadConnected {
                    slot: 3,
                    kind: GamepadKind::Xbox
                }
            ]
        );
        pads.set_slot(None);
        assert_eq!(
            link.take(),
            vec![InputEvent::GamepadDisconnected { slot: 3 }]
        );
        assert_eq!(pads.active(), None);
    }

    #[test]
    fn buttons_go_once_per_change_with_their_time() {
        let (mut pads, link, _) = connected();
        let at = Instant::now();
        assert!(pads.on_key(7, keycode::KEYCODE_BUTTON_A, true, at));
        // La repetición automática no se manda.
        assert!(pads.on_key(7, keycode::KEYCODE_BUTTON_A, true, at));
        assert!(pads.on_key(7, keycode::KEYCODE_BACK, true, at));
        assert!(pads.on_key(7, keycode::KEYCODE_BUTTON_A, false, at));
        let events = std::mem::take(&mut *link.events.lock().unwrap());
        assert_eq!(
            events,
            vec![
                (pressed(2, GamepadButton::A, true), Some(at)),
                (pressed(2, GamepadButton::Back, true), Some(at)),
                (pressed(2, GamepadButton::A, false), Some(at)),
            ]
        );
        // Una tecla que no es de mando, o un dispositivo que no es un mando: no son de aquí.
        assert!(!pads.on_key(7, 29, true, at));
        assert!(!pads.on_key(99, keycode::KEYCODE_BUTTON_A, true, at));
        assert!(!pads.on_motion(99, &PadAxes::default(), at));
        assert!(link.take().is_empty());
    }

    #[test]
    fn sticks_have_the_gilrs_values_and_y_points_up() {
        let (mut pads, link, _) = connected();
        let at = Instant::now();
        let mut axes = PadAxes {
            x: 1.0,
            y: -1.0,
            z: -0.5,
            rz: 0.5,
            ..PadAxes::default()
        };
        assert!(pads.on_motion(7, &axes, at));
        assert_eq!(
            link.take(),
            vec![
                axis(2, GamepadAxis::LeftX, i16::MAX),
                axis(2, GamepadAxis::LeftY, i16::MAX),
                axis(2, GamepadAxis::RightX, stick_value(-0.5)),
                axis(2, GamepadAxis::RightY, stick_value(-0.5)),
            ]
        );
        // Un cambio por debajo del umbral no sale; la zona muerta lleva al reposo, que sí.
        axes.x = 0.999;
        pads.on_motion(7, &axes, at);
        assert!(link.take().is_empty());
        axes.x = 0.01;
        pads.on_motion(7, &axes, at);
        assert_eq!(link.take(), vec![axis(2, GamepadAxis::LeftX, 0)]);
        // NaN es reposo.
        axes.y = f32::NAN;
        pads.on_motion(7, &axes, at);
        assert_eq!(link.take(), vec![axis(2, GamepadAxis::LeftY, 0)]);
    }

    #[test]
    fn triggers_on_both_axes_go_once() {
        let (mut pads, link, _) = connected();
        let at = Instant::now();
        // Un mando que da el gatillo en `LTRIGGER` y en `BRAKE` a la vez: un solo eje.
        let both = PadAxes {
            ltrigger: 0.5,
            brake: 0.5,
            rtrigger: 1.0,
            gas: 1.0,
            ..PadAxes::default()
        };
        pads.on_motion(7, &both, at);
        assert_eq!(
            link.take(),
            vec![
                axis(2, GamepadAxis::LeftTrigger, trigger_value(0.5)),
                axis(2, GamepadAxis::RightTrigger, i16::MAX),
            ]
        );
        pads.on_motion(7, &both, at);
        assert!(link.take().is_empty());
        // Otro que solo los da en `BRAKE` y `GAS`.
        let only_brake = PadAxes {
            brake: 0.25,
            ..PadAxes::default()
        };
        pads.on_motion(7, &only_brake, at);
        assert_eq!(
            link.take(),
            vec![
                axis(2, GamepadAxis::LeftTrigger, trigger_value(0.25)),
                axis(2, GamepadAxis::RightTrigger, 0),
            ]
        );
        // Con gatillos analógicos, las teclas `L2`/`R2` que manda el mismo mando se ignoran.
        assert!(pads.on_key(7, keycode::KEYCODE_BUTTON_L2, true, at));
        assert!(link.take().is_empty());
    }

    #[test]
    fn digital_triggers_are_full_axes() {
        let (mut pads, link, _) = connected();
        let at = Instant::now();
        assert!(pads.on_key(7, keycode::KEYCODE_BUTTON_R2, true, at));
        assert_eq!(
            link.take(),
            vec![axis(2, GamepadAxis::RightTrigger, i16::MAX)]
        );
        // Un `MotionEvent` sin gatillos no lo suelta.
        pads.on_motion(7, &PadAxes::default(), at);
        assert!(link.take().is_empty());
        assert!(pads.on_key(7, keycode::KEYCODE_BUTTON_R2, false, at));
        assert_eq!(link.take(), vec![axis(2, GamepadAxis::RightTrigger, 0)]);
    }

    #[test]
    fn the_dpad_as_keys_or_hat_goes_once() {
        let (mut pads, link, _) = connected();
        let at = Instant::now();
        // Como eje.
        let right = PadAxes {
            hat_x: 1.0,
            ..PadAxes::default()
        };
        pads.on_motion(7, &right, at);
        pads.on_motion(7, &right, at);
        pads.on_motion(7, &PadAxes::default(), at);
        assert_eq!(
            link.take(),
            vec![
                pressed(2, GamepadButton::DPadRight, true),
                pressed(2, GamepadButton::DPadRight, false),
            ]
        );
        // Como teclas.
        assert!(pads.on_key(7, keycode::KEYCODE_DPAD_LEFT, true, at));
        assert!(pads.on_key(7, keycode::KEYCODE_DPAD_LEFT, false, at));
        assert_eq!(
            link.take(),
            vec![
                pressed(2, GamepadButton::DPadLeft, true),
                pressed(2, GamepadButton::DPadLeft, false),
            ]
        );
        // Las dos a la vez (el eje y la tecla que Android sintetiza de él): una pulsación, que
        // se suelta cuando se sueltan las dos.
        let up = PadAxes {
            hat_y: -1.0,
            ..PadAxes::default()
        };
        pads.on_motion(7, &up, at);
        pads.on_key(7, keycode::KEYCODE_DPAD_UP, true, at);
        pads.on_key(7, keycode::KEYCODE_DPAD_UP, false, at);
        assert_eq!(link.take(), vec![pressed(2, GamepadButton::DPadUp, true)]);
        pads.on_motion(7, &PadAxes::default(), at);
        assert_eq!(link.take(), vec![pressed(2, GamepadButton::DPadUp, false)]);
        // Abajo por el eje.
        pads.on_motion(
            7,
            &PadAxes {
                hat_y: 1.0,
                ..PadAxes::default()
            },
            at,
        );
        assert_eq!(link.take(), vec![pressed(2, GamepadButton::DPadDown, true)]);
    }

    #[test]
    fn the_choice_picks_the_pad_and_follows_hotplug() {
        let (mut pads, link, _) = pads();
        pads.set_slot(Some(1));
        pads.device_added(10, key("Primero"));
        assert_eq!(pads.active(), Some((1, 10)));
        // En automático, el último conectado.
        pads.device_added(11, key("Segundo"));
        assert_eq!(pads.active(), Some((1, 11)));
        assert_eq!(
            link.take(),
            vec![
                InputEvent::GamepadConnected {
                    slot: 1,
                    kind: GamepadKind::Xbox
                },
                InputEvent::GamepadDisconnected { slot: 1 },
                InputEvent::GamepadConnected {
                    slot: 1,
                    kind: GamepadKind::Xbox
                },
            ]
        );
        // Lo del que no va en la ranura no sale (pero es de un mando: no va al teclado).
        let at = Instant::now();
        assert!(pads.on_key(10, keycode::KEYCODE_BUTTON_A, true, at));
        assert!(pads.on_motion(
            10,
            &PadAxes {
                x: 1.0,
                ..PadAxes::default()
            },
            at
        ));
        assert!(link.take().is_empty());
        // El elegido, aunque no sea el último.
        pads.set_choice(PadChoice::Pad(key("Primero")));
        assert_eq!(pads.active(), Some((1, 10)));
        assert_eq!(link.take().len(), 2);
        // Se desconecta el elegido: la ranura se queda sin mando hasta que vuelva.
        pads.device_removed(10);
        assert_eq!(pads.active(), None);
        assert_eq!(
            link.take(),
            vec![InputEvent::GamepadDisconnected { slot: 1 }]
        );
        pads.device_removed(10);
        assert!(link.take().is_empty());
        pads.device_added(12, key("Primero"));
        assert_eq!(pads.active(), Some((1, 12)));
        // En automático, si se va el último, el anterior que siga.
        pads.set_choice(PadChoice::Auto);
        assert_eq!(pads.active(), Some((1, 12)));
        pads.device_removed(12);
        assert_eq!(pads.active(), Some((1, 11)));
        // El mando que vuelve no trae botones de antes: los ejes se vuelven a mandar.
        link.take();
        pads.on_motion(
            11,
            &PadAxes {
                x: 1.0,
                ..PadAxes::default()
            },
            at,
        );
        assert_eq!(link.take(), vec![axis(1, GamepadAxis::LeftX, i16::MAX)]);
    }

    #[test]
    fn host_rumble_reaches_the_app_motors_and_stops() {
        let (mut pads, link, motors) = connected();
        // Sin nada pedido, no se toca el mando.
        pads.poll();
        assert!(motors.set.lock().unwrap().is_empty());
        *link.rumble.lock().unwrap() = Rumble::new(0xFFFF, 0x8000);
        pads.poll();
        pads.poll();
        *link.rumble.lock().unwrap() = Rumble::new(0x1000, 0);
        pads.poll();
        // El mando deja la ranura: parado.
        pads.set_slot(None);
        pads.poll();
        assert_eq!(
            *motors.set.lock().unwrap(),
            vec![
                (7, 2, Rumble::new(0xFFFF, 0x8000)),
                (7, 2, Rumble::new(0x1000, 0)),
                (7, 2, Rumble::OFF),
            ]
        );
        // Al terminar, también.
        motors.set.lock().unwrap().clear();
        pads.set_slot(Some(4));
        pads.poll();
        drop(pads);
        assert_eq!(
            *motors.set.lock().unwrap(),
            vec![(7, 4, Rumble::new(0x1000, 0)), (7, 4, Rumble::OFF)]
        );
    }

    #[test]
    fn a_pad_without_a_vibrator_is_tried_and_left_alone() {
        let link = Arc::new(Link::default());
        let motors = AppMotors {
            unsupported: true,
            ..AppMotors::default()
        };
        let mut pads = AndroidPads::new(
            Arc::clone(&link) as Arc<dyn PadLink>,
            PadChoice::Auto,
            Box::new(motors.clone()),
        );
        pads.device_added(7, key("Mando sin motores"));
        pads.set_slot(Some(1));
        *link.rumble.lock().unwrap() = Rumble::new(0xFFFF, 0xFFFF);
        // Falla (y se avisa una vez); nada se queda puesto ni se cuelga.
        pads.poll();
        pads.poll();
        pads.set_slot(None);
        assert!(motors.set.lock().unwrap().is_empty());
    }
}
