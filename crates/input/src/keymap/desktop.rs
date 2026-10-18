//! La tabla de las teclas de `winit` (no en Android) y las búsquedas que salen de ella.

use winit::keyboard::{KeyCode, NativeKeyCode, PhysicalKey};

use super::{Scancode, evdev};

/// La lista fija: tecla de `winit`, código evdev y *scancode* set 1.
pub const KEYS: &[(KeyCode, u16, u16)] = &[
    (KeyCode::Escape, 1, 0x0001),
    (KeyCode::Digit1, 2, 0x0002),
    (KeyCode::Digit2, 3, 0x0003),
    (KeyCode::Digit3, 4, 0x0004),
    (KeyCode::Digit4, 5, 0x0005),
    (KeyCode::Digit5, 6, 0x0006),
    (KeyCode::Digit6, 7, 0x0007),
    (KeyCode::Digit7, 8, 0x0008),
    (KeyCode::Digit8, 9, 0x0009),
    (KeyCode::Digit9, 10, 0x000a),
    (KeyCode::Digit0, 11, 0x000b),
    (KeyCode::Minus, 12, 0x000c),
    (KeyCode::Equal, 13, 0x000d),
    (KeyCode::Backspace, 14, 0x000e),
    (KeyCode::Tab, 15, 0x000f),
    (KeyCode::KeyQ, 16, 0x0010),
    (KeyCode::KeyW, 17, 0x0011),
    (KeyCode::KeyE, 18, 0x0012),
    (KeyCode::KeyR, 19, 0x0013),
    (KeyCode::KeyT, 20, 0x0014),
    (KeyCode::KeyY, 21, 0x0015),
    (KeyCode::KeyU, 22, 0x0016),
    (KeyCode::KeyI, 23, 0x0017),
    (KeyCode::KeyO, 24, 0x0018),
    (KeyCode::KeyP, 25, 0x0019),
    (KeyCode::BracketLeft, 26, 0x001a),
    (KeyCode::BracketRight, 27, 0x001b),
    (KeyCode::Enter, 28, 0x001c),
    (KeyCode::ControlLeft, 29, 0x001d),
    (KeyCode::KeyA, 30, 0x001e),
    (KeyCode::KeyS, 31, 0x001f),
    (KeyCode::KeyD, 32, 0x0020),
    (KeyCode::KeyF, 33, 0x0021),
    (KeyCode::KeyG, 34, 0x0022),
    (KeyCode::KeyH, 35, 0x0023),
    (KeyCode::KeyJ, 36, 0x0024),
    (KeyCode::KeyK, 37, 0x0025),
    (KeyCode::KeyL, 38, 0x0026),
    (KeyCode::Semicolon, 39, 0x0027),
    (KeyCode::Quote, 40, 0x0028),
    (KeyCode::Backquote, 41, 0x0029),
    (KeyCode::ShiftLeft, 42, 0x002a),
    (KeyCode::Backslash, 43, 0x002b),
    (KeyCode::KeyZ, 44, 0x002c),
    (KeyCode::KeyX, 45, 0x002d),
    (KeyCode::KeyC, 46, 0x002e),
    (KeyCode::KeyV, 47, 0x002f),
    (KeyCode::KeyB, 48, 0x0030),
    (KeyCode::KeyN, 49, 0x0031),
    (KeyCode::KeyM, 50, 0x0032),
    (KeyCode::Comma, 51, 0x0033),
    (KeyCode::Period, 52, 0x0034),
    (KeyCode::Slash, 53, 0x0035),
    (KeyCode::ShiftRight, 54, 0x0036),
    (KeyCode::NumpadMultiply, 55, 0x0037),
    (KeyCode::AltLeft, 56, 0x0038),
    (KeyCode::Space, 57, 0x0039),
    (KeyCode::CapsLock, 58, 0x003a),
    (KeyCode::F1, 59, 0x003b),
    (KeyCode::F2, 60, 0x003c),
    (KeyCode::F3, 61, 0x003d),
    (KeyCode::F4, 62, 0x003e),
    (KeyCode::F5, 63, 0x003f),
    (KeyCode::F6, 64, 0x0040),
    (KeyCode::F7, 65, 0x0041),
    (KeyCode::F8, 66, 0x0042),
    (KeyCode::F9, 67, 0x0043),
    (KeyCode::F10, 68, 0x0044),
    (KeyCode::NumLock, 69, 0xe045),
    (KeyCode::ScrollLock, 70, 0x0046),
    (KeyCode::Numpad7, 71, 0x0047),
    (KeyCode::Numpad8, 72, 0x0048),
    (KeyCode::Numpad9, 73, 0x0049),
    (KeyCode::NumpadSubtract, 74, 0x004a),
    (KeyCode::Numpad4, 75, 0x004b),
    (KeyCode::Numpad5, 76, 0x004c),
    (KeyCode::Numpad6, 77, 0x004d),
    (KeyCode::NumpadAdd, 78, 0x004e),
    (KeyCode::Numpad1, 79, 0x004f),
    (KeyCode::Numpad2, 80, 0x0050),
    (KeyCode::Numpad3, 81, 0x0051),
    (KeyCode::Numpad0, 82, 0x0052),
    (KeyCode::NumpadDecimal, 83, 0x0053),
    (KeyCode::IntlBackslash, 86, 0x0056),
    (KeyCode::F11, 87, 0x0057),
    (KeyCode::F12, 88, 0x0058),
    (KeyCode::IntlRo, 89, 0x0073),
    (KeyCode::Convert, 92, 0x0079),
    (KeyCode::KanaMode, 93, 0x0070),
    (KeyCode::NonConvert, 94, 0x007b),
    (KeyCode::NumpadEnter, 96, 0xe01c),
    (KeyCode::ControlRight, 97, 0xe01d),
    (KeyCode::NumpadDivide, 98, 0xe035),
    (KeyCode::PrintScreen, 99, 0xe037),
    (KeyCode::AltRight, 100, 0xe038),
    (KeyCode::Home, 102, 0xe047),
    (KeyCode::ArrowUp, 103, 0xe048),
    (KeyCode::PageUp, 104, 0xe049),
    (KeyCode::ArrowLeft, 105, 0xe04b),
    (KeyCode::ArrowRight, 106, 0xe04d),
    (KeyCode::End, 107, 0xe04f),
    (KeyCode::ArrowDown, 108, 0xe050),
    (KeyCode::PageDown, 109, 0xe051),
    (KeyCode::Insert, 110, 0xe052),
    (KeyCode::Delete, 111, 0xe053),
    (KeyCode::AudioVolumeMute, 113, 0xe020),
    (KeyCode::AudioVolumeDown, 114, 0xe02e),
    (KeyCode::AudioVolumeUp, 115, 0xe030),
    (KeyCode::Power, 116, 0xe05e),
    (KeyCode::NumpadEqual, 117, 0x0059),
    (KeyCode::Pause, 119, 0x0045),
    (KeyCode::NumpadComma, 121, 0x007e),
    (KeyCode::Lang1, 122, 0x0072),
    (KeyCode::Lang2, 123, 0x0071),
    (KeyCode::IntlYen, 124, 0x007d),
    (KeyCode::SuperLeft, 125, 0xe05b),
    (KeyCode::SuperRight, 126, 0xe05c),
    (KeyCode::ContextMenu, 127, 0xe05d),
    (KeyCode::BrowserStop, 128, 0xe068),
    (KeyCode::LaunchApp2, 140, 0xe021),
    (KeyCode::Sleep, 142, 0xe05f),
    (KeyCode::WakeUp, 143, 0xe063),
    (KeyCode::LaunchMail, 155, 0xe06c),
    (KeyCode::BrowserFavorites, 156, 0xe066),
    (KeyCode::LaunchApp1, 157, 0xe06b),
    (KeyCode::BrowserBack, 158, 0xe06a),
    (KeyCode::BrowserForward, 159, 0xe069),
    (KeyCode::MediaTrackNext, 163, 0xe019),
    (KeyCode::MediaPlayPause, 164, 0xe022),
    (KeyCode::MediaTrackPrevious, 165, 0xe010),
    (KeyCode::MediaStop, 166, 0xe024),
    (KeyCode::BrowserHome, 172, 0xe032),
    (KeyCode::BrowserRefresh, 173, 0xe067),
    (KeyCode::F13, 183, 0x0064),
    (KeyCode::F14, 184, 0x0065),
    (KeyCode::F15, 185, 0x0066),
    (KeyCode::F16, 186, 0x0067),
    (KeyCode::F17, 187, 0x0068),
    (KeyCode::F18, 188, 0x0069),
    (KeyCode::F19, 189, 0x006a),
    (KeyCode::F20, 190, 0x006b),
    (KeyCode::F21, 191, 0x006c),
    (KeyCode::F22, 192, 0x006d),
    (KeyCode::F23, 193, 0x006e),
    (KeyCode::F24, 194, 0x0076),
    (KeyCode::BrowserSearch, 217, 0xe065),
    (KeyCode::MediaSelect, 226, 0xe06d),
];

/// Posición de un *scancode* en la tabla inversa: 0..256 sin prefijo, 256..512 con `E0`.
const fn scancode_slot(scancode: u16) -> Option<usize> {
    let code = (scancode & 0xff) as usize;
    match scancode >> 8 {
        0x00 => Some(code),
        0xe0 => Some(256 + code),
        _ => None,
    }
}

/// evdev → *scancode* (0 = sin *scancode*), generada de [`KEYS`].
const EVDEV_TO_SCANCODE: [u16; evdev::KEY_LIMIT] = {
    let mut table = [0u16; evdev::KEY_LIMIT];
    let mut i = 0;
    while i < KEYS.len() {
        table[KEYS[i].1 as usize] = KEYS[i].2;
        i += 1;
    }
    table
};

/// *Scancode* → evdev (0 = sin tecla), generada de [`KEYS`].
const SCANCODE_TO_EVDEV: [u16; 512] = {
    let mut table = [0u16; 512];
    let mut i = 0;
    while i < KEYS.len() {
        match scancode_slot(KEYS[i].2) {
            Some(slot) => table[slot] = KEYS[i].1,
            None => panic!("scancode con prefijo desconocido en KEYS"),
        }
        i += 1;
    }
    table
};

/// Código evdev de una tecla de `winit`, si está en la tabla.
pub fn evdev_from_keycode(code: KeyCode) -> Option<u16> {
    KEYS.iter()
        .find(|(key, _, _)| *key == code)
        .map(|(_, evdev, _)| *evdev)
}

/// Tecla de `winit` de un código evdev, si está en la tabla.
pub fn keycode_from_evdev(code: u16) -> Option<KeyCode> {
    KEYS.iter()
        .find(|(_, evdev, _)| *evdev == code)
        .map(|(key, _, _)| *key)
}

/// Tecla por su nombre (`--ptt TECLA`): una letra (`v`), una cifra (`5`) o el nombre de `winit`
/// sin distinguir mayúsculas (`F12`, `CapsLock`, `Backquote`, `ControlRight`…).
pub fn keycode_from_name(name: &str) -> Option<KeyCode> {
    let name = name.trim();
    let mut chars = name.chars();
    let wanted = match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => format!("Key{}", c.to_ascii_uppercase()),
        (Some(c), None) if c.is_ascii_digit() => format!("Digit{c}"),
        _ => name.to_owned(),
    };
    KEYS.iter()
        .map(|(key, _, _)| *key)
        .find(|key| format!("{key:?}").eq_ignore_ascii_case(&wanted))
}

/// Código evdev de una tecla física: por la tabla o, si `winit` no la identificó, por el código
/// nativo (XKB menos 8 en Linux, *scancode* en Windows).
pub fn evdev_from_physical(key: PhysicalKey) -> Option<u16> {
    match key {
        PhysicalKey::Code(code) => evdev_from_keycode(code),
        PhysicalKey::Unidentified(NativeKeyCode::Xkb(raw)) => u16::try_from(raw)
            .ok()
            .and_then(|raw| raw.checked_sub(8))
            .filter(|code| *code > 0 && usize::from(*code) < evdev::KEY_LIMIT),
        PhysicalKey::Unidentified(NativeKeyCode::Windows(scancode)) => {
            evdev_from_scancode(Scancode(scancode))
        }
        PhysicalKey::Unidentified(_) => None,
    }
}

/// *Scancode* set 1 de un código evdev.
pub fn scancode_from_evdev(code: u16) -> Option<Scancode> {
    EVDEV_TO_SCANCODE
        .get(usize::from(code))
        .copied()
        .filter(|scancode| *scancode != 0)
        .map(Scancode)
}

/// Código evdev de un *scancode* set 1.
pub fn evdev_from_scancode(scancode: Scancode) -> Option<u16> {
    scancode_slot(scancode.0)
        .map(|slot| SCANCODE_TO_EVDEV[slot])
        .filter(|code| *code != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_found_by_name() {
        assert_eq!(keycode_from_name("v"), Some(KeyCode::KeyV));
        assert_eq!(keycode_from_name("5"), Some(KeyCode::Digit5));
        assert_eq!(keycode_from_name("f12"), Some(KeyCode::F12));
        assert_eq!(keycode_from_name(" CapsLock "), Some(KeyCode::CapsLock));
        assert_eq!(keycode_from_name("Backquote"), Some(KeyCode::Backquote));
        assert_eq!(keycode_from_name("tecla"), None);
    }
    use std::collections::HashSet;

    #[test]
    fn the_table_has_no_repeated_codes() {
        let keys: HashSet<KeyCode> = KEYS.iter().map(|(key, _, _)| *key).collect();
        let evdevs: HashSet<u16> = KEYS.iter().map(|(_, evdev, _)| *evdev).collect();
        let scancodes: HashSet<u16> = KEYS.iter().map(|(_, _, sc)| *sc).collect();
        assert_eq!(keys.len(), KEYS.len(), "teclas de winit repetidas");
        assert_eq!(evdevs.len(), KEYS.len(), "códigos evdev repetidos");
        assert_eq!(scancodes.len(), KEYS.len(), "scancodes repetidos");
        for (key, evdev, scancode) in KEYS {
            assert!(
                *evdev > 0 && usize::from(*evdev) < evdev::KEY_LIMIT,
                "{key:?}"
            );
            assert!(
                matches!(scancode >> 8, 0x00 | 0xe0) && scancode & 0xff != 0,
                "{key:?}: {scancode:#06x}"
            );
        }
    }

    #[test]
    fn evdev_and_scancodes_round_trip() {
        for (key, evdev, scancode) in KEYS {
            assert_eq!(evdev_from_keycode(*key), Some(*evdev));
            assert_eq!(keycode_from_evdev(*evdev), Some(*key));
            assert_eq!(scancode_from_evdev(*evdev), Some(Scancode(*scancode)));
            assert_eq!(evdev_from_scancode(Scancode(*scancode)), Some(*evdev));
            assert_eq!(
                evdev_from_physical(PhysicalKey::Code(*key)),
                Some(*evdev),
                "{key:?}"
            );
            assert_eq!(
                evdev_from_physical(PhysicalKey::Unidentified(NativeKeyCode::Windows(*scancode))),
                Some(*evdev)
            );
            assert_eq!(
                evdev_from_physical(PhysicalKey::Unidentified(NativeKeyCode::Xkb(
                    u32::from(*evdev) + 8
                ))),
                Some(*evdev)
            );
        }
        assert_eq!(scancode_from_evdev(0), None);
        assert_eq!(scancode_from_evdev(999), None);
        assert_eq!(evdev_from_scancode(Scancode(0xe1_1d)), None);
    }

    #[test]
    fn a_full_iso_keyboard_is_covered() {
        // Las 105 teclas de un teclado ISO completo (ANSI son 104: sin `IntlBackslash`).
        let iso = [
            KeyCode::Escape,
            KeyCode::F1,
            KeyCode::F2,
            KeyCode::F3,
            KeyCode::F4,
            KeyCode::F5,
            KeyCode::F6,
            KeyCode::F7,
            KeyCode::F8,
            KeyCode::F9,
            KeyCode::F10,
            KeyCode::F11,
            KeyCode::F12,
            KeyCode::PrintScreen,
            KeyCode::ScrollLock,
            KeyCode::Pause,
            KeyCode::Backquote,
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
            KeyCode::Digit0,
            KeyCode::Minus,
            KeyCode::Equal,
            KeyCode::Backspace,
            KeyCode::Insert,
            KeyCode::Home,
            KeyCode::PageUp,
            KeyCode::NumLock,
            KeyCode::NumpadDivide,
            KeyCode::NumpadMultiply,
            KeyCode::NumpadSubtract,
            KeyCode::Tab,
            KeyCode::KeyQ,
            KeyCode::KeyW,
            KeyCode::KeyE,
            KeyCode::KeyR,
            KeyCode::KeyT,
            KeyCode::KeyY,
            KeyCode::KeyU,
            KeyCode::KeyI,
            KeyCode::KeyO,
            KeyCode::KeyP,
            KeyCode::BracketLeft,
            KeyCode::BracketRight,
            KeyCode::Enter,
            KeyCode::Delete,
            KeyCode::End,
            KeyCode::PageDown,
            KeyCode::Numpad7,
            KeyCode::Numpad8,
            KeyCode::Numpad9,
            KeyCode::NumpadAdd,
            KeyCode::CapsLock,
            KeyCode::KeyA,
            KeyCode::KeyS,
            KeyCode::KeyD,
            KeyCode::KeyF,
            KeyCode::KeyG,
            KeyCode::KeyH,
            KeyCode::KeyJ,
            KeyCode::KeyK,
            KeyCode::KeyL,
            KeyCode::Semicolon,
            KeyCode::Quote,
            KeyCode::Backslash,
            KeyCode::Numpad4,
            KeyCode::Numpad5,
            KeyCode::Numpad6,
            KeyCode::ShiftLeft,
            KeyCode::IntlBackslash,
            KeyCode::KeyZ,
            KeyCode::KeyX,
            KeyCode::KeyC,
            KeyCode::KeyV,
            KeyCode::KeyB,
            KeyCode::KeyN,
            KeyCode::KeyM,
            KeyCode::Comma,
            KeyCode::Period,
            KeyCode::Slash,
            KeyCode::ShiftRight,
            KeyCode::ArrowUp,
            KeyCode::Numpad1,
            KeyCode::Numpad2,
            KeyCode::Numpad3,
            KeyCode::NumpadEnter,
            KeyCode::ControlLeft,
            KeyCode::SuperLeft,
            KeyCode::AltLeft,
            KeyCode::Space,
            KeyCode::AltRight,
            KeyCode::SuperRight,
            KeyCode::ContextMenu,
            KeyCode::ControlRight,
            KeyCode::ArrowLeft,
            KeyCode::ArrowDown,
            KeyCode::ArrowRight,
            KeyCode::Numpad0,
            KeyCode::NumpadDecimal,
        ];
        assert_eq!(iso.len(), 105);
        for key in iso {
            let evdev = evdev_from_keycode(key).unwrap_or_else(|| panic!("falta {key:?}"));
            assert!(scancode_from_evdev(evdev).is_some(), "{key:?} sin scancode");
        }
        let extended: Vec<KeyCode> = iso
            .iter()
            .copied()
            .filter(|key| {
                evdev_from_keycode(*key)
                    .and_then(scancode_from_evdev)
                    .is_some_and(Scancode::extended)
            })
            .collect();
        // Las extendidas de un teclado de 105 teclas.
        assert_eq!(extended.len(), 19, "{extended:?}");
    }

    /// La tabla coincide con la propia de `winit` en esta plataforma (evdev en Linux, *scancode*
    /// en Windows) para todas las teclas que `winit` sabe convertir.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn the_table_agrees_with_winit() {
        use winit::platform::scancode::PhysicalKeyExtScancode;
        let mut compared = 0;
        for (key, evdev, scancode) in KEYS {
            let Some(native) = key.to_scancode() else {
                continue;
            };
            let expected = if cfg!(target_os = "linux") {
                u32::from(*evdev)
            } else {
                u32::from(*scancode)
            };
            assert_eq!(native, expected, "{key:?}");
            compared += 1;
        }
        assert!(compared >= 100, "solo {compared} teclas comparadas");
    }
}
