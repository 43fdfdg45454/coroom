//! El mapa XKB que anuncia el teclado de EI y cómo teclear con él un carácter sin tecla física
//! ([`InputEvent::Text`](crate::InputEvent::Text): la ñ, la á o un emoji del teclado en pantalla
//! de Android).
//!
//! El compositor dice en `ei_keyboard.keymap` qué distribución usa el teclado virtual: un mapa XKB
//! de texto por un fd ([`TextKeymap::from_fd`]). Al recibirlo se indexa, por cada distribución
//! (grupo) del mapa, qué teclas dan cada keysym en algún nivel, qué teclas son modificadores (y qué
//! máscara ponen, comprobada en un `xkb_state`) y qué caracteres se componen con una **tecla
//! muerta** y otra (`dead_acute` + `a` = «á»): cada par de una tecla muerta de la distribución y un
//! carácter de la distribución se pasa por la tabla de composición del *locale* del anfitrión
//! (`LC_ALL`, `LC_CTYPE` o `LANG`, si no `C`; la misma que usan las aplicaciones, `~/.XCompose`
//! incluido) y se apunta lo que dé. Un mapa nuevo llega con un dispositivo nuevo, que se indexa de
//! nuevo.
//!
//! Cada búsqueda ([`TextKeymap::lookup`]) se hace **contra el estado actual del teclado**: la
//! distribución activa y los modificadores bloqueados y enganchados (`ei_keyboard.modifiers`: Bloq
//! Mayús, Bloq Num…) y los que tienen pulsados los invitados (que no se sueltan). Entre las teclas
//! que dan el keysym del carácter (`xkb_utf32_to_keysym`, o su forma Unicode directa `U+XXXX`) y las
//! combinaciones de Mayúsculas y AltGr (`ISO_Level3_Shift`) que se pueden añadir, vale la que en un
//! `xkb_state` con ese estado da **exactamente** ese keysym y ese carácter (con Control pulsado,
//! por ejemplo, ninguna); gana la que pide pulsar menos modificadores y, a igualdad, la de código
//! más bajo, con las de código por encima de 255 (que XWayland no entrega a sus clientes) al final.
//! Si el carácter no tiene tecla, se busca igual una tecla muerta y otra que lo compongan. Si no hay
//! ninguna forma, `None`: no se puede teclear.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;

use xkbcommon::xkb;
use xkbcommon::xkb::compose;

use crate::keymap::evdev as codes;

/// Tamaño máximo de un mapa que se acepta del compositor (los reales rondan los 100 KiB).
const MAX_KEYMAP_BYTES: u32 = 16 * 1024 * 1024;

/// Distancia entre el código de tecla de XKB y el de evdev.
const EVDEV_OFFSET: u32 = 8;

/// Mayor código de tecla de X11: los de encima (como el `KEY_EURO` de algunos mapas) no llegan a
/// los clientes de XWayland, que es por donde van muchos juegos.
const MAX_X11_KEYCODE: u32 = 255;

/// Forma Unicode directa de los keysyms (`U+XXXX`).
const UNICODE_KEYSYM: u32 = 0x0100_0000;

/// Keysyms de las teclas muertas (`dead_grave` … `dead_lowline` y siguientes).
const DEAD_KEYSYMS: std::ops::RangeInclusive<u32> = 0xfe50..=0xfe93;

/// Qué tabla de composición usar para las teclas muertas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compose<'a> {
    /// La del *locale* de este proceso (`LC_ALL`, `LC_CTYPE` o `LANG`; si no, `C`).
    Host,
    /// La de un *locale* concreto.
    Locale(&'a str),
    /// Las reglas de un fichero `Compose` (para los tests).
    Rules(&'a str),
    /// Ninguna: solo lo que tiene tecla.
    None,
}

/// Estado del teclado del compositor contra el que se busca (de `ei_keyboard.modifiers`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyboardState {
    /// Distribución activa, desde 0.
    pub group: u32,
    /// Modificadores enganchados.
    pub latched: xkb::ModMask,
    /// Modificadores bloqueados (Bloq Mayús, Bloq Num…).
    pub locked: xkb::ModMask,
}

/// Una pulsación: los modificadores que hay que pulsar antes (los que no estaban ya) y la tecla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stroke {
    /// La tecla, código evdev (el de EI).
    pub key: u16,
    /// La tecla de Mayúsculas que hay que pulsar, si hace falta y no estaba pulsada.
    pub shift: Option<u16>,
    /// La de AltGr, igual.
    pub level3: Option<u16>,
}

impl Stroke {
    /// Orden de preferencia: primero las teclas que ve XWayland, después las que piden pulsar
    /// menos modificadores y, a igualdad, la de código más bajo.
    fn rank(&self) -> (bool, u8, u16) {
        (
            u32::from(self.key) + EVDEV_OFFSET > MAX_X11_KEYCODE,
            u8::from(self.shift.is_some()) + u8::from(self.level3.is_some()),
            self.key,
        )
    }

    /// Los modificadores que pulsa, en orden.
    pub fn modifiers(&self) -> impl DoubleEndedIterator<Item = u16> + Clone {
        [self.shift, self.level3].into_iter().flatten()
    }
}

/// Cómo teclear un carácter: una pulsación o, con tecla muerta, dos seguidas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Typing {
    strokes: [Stroke; 2],
    len: usize,
}

impl Typing {
    fn one(stroke: Stroke) -> Self {
        Self {
            strokes: [stroke; 2],
            len: 1,
        }
    }

    fn two(dead: Stroke, base: Stroke) -> Self {
        Self {
            strokes: [dead, base],
            len: 2,
        }
    }

    /// Las pulsaciones, en orden (cada una se suelta antes de la siguiente).
    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes[..self.len]
    }

    fn rank(&self) -> (bool, u8, u16) {
        self.strokes()
            .iter()
            .map(Stroke::rank)
            .fold((false, 0, 0), |(far, mods, key), (f, m, k)| {
                (far || f, mods + m, key.max(k))
            })
    }
}

/// Una distribución del mapa.
#[derive(Debug, Default)]
struct Group {
    /// Por keysym, las teclas que lo dan en algún nivel, la preferida primero.
    keys: HashMap<u32, Vec<u16>>,
    /// Por carácter, los pares (tecla muerta, keysym) que la tabla de composición convierte en él.
    composed: HashMap<char, Vec<(u32, u32)>>,
    /// Teclas de modificador (las que ponen alguna máscara pulsadas solas) y su máscara.
    modifiers: Vec<(u16, xkb::ModMask)>,
    /// Teclas que ponen Mayúsculas, la preferida primero, y su máscara.
    shift: Vec<u16>,
    shift_mask: xkb::ModMask,
    /// Teclas que ponen AltGr, la preferida primero, y su máscara.
    level3: Vec<u16>,
    level3_mask: xkb::ModMask,
}

/// El mapa compilado, con un solo dueño.
struct OwnedKeymap(xkb::Keymap);

// SAFETY: libxkbcommon no guarda estado por hilo; lo que no admite es usar el mismo objeto desde
// dos hilos a la vez (sus cuentas de referencias no son atómicas). Este mapa no se clona nunca, su
// contexto es solo suyo (se crea para él en `TextKeymap::build` y el que se creó allí se suelta
// antes de devolverlo), y los `xkb_state` que lo referencian viven solo dentro de una búsqueda con
// `&self`. Como `OwnedKeymap` no es `Sync`, mover el `TextKeymap` a otro hilo (el hilo `ei` lo crea
// y el hilo `inject` lo consulta, siempre tras el mutex de `EiState`) no permite dos usos a la vez.
#[allow(unsafe_code)]
unsafe impl Send for OwnedKeymap {}

impl std::fmt::Debug for OwnedKeymap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keymap")
    }
}

/// Qué tecla, qué modificadores y qué tecla muerta dan cada carácter en cada distribución de un
/// mapa XKB.
#[derive(Debug)]
pub struct TextKeymap {
    keymap: OwnedKeymap,
    groups: Vec<Group>,
}

impl TextKeymap {
    /// Lee el mapa del fd de `ei_keyboard.keymap` (`size` bytes, el último puede ser un NUL), con
    /// la tabla de composición del anfitrión.
    pub fn from_fd(fd: &OwnedFd, size: u32) -> Result<Self, String> {
        if size == 0 || size > MAX_KEYMAP_BYTES {
            return Err(format!("mapa de teclado de {size} bytes"));
        }
        let file = std::fs::File::from(
            fd.try_clone()
                .map_err(|err| format!("no se pudo duplicar el fd del mapa: {err}"))?,
        );
        let mut bytes = vec![0u8; size as usize];
        file.read_exact_at(&mut bytes, 0)
            .map_err(|err| format!("no se pudo leer el mapa de teclado: {err}"))?;
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| "el mapa de teclado no es texto UTF-8".to_owned())?;
        Self::from_string(text, Compose::Host)
    }

    /// Compila un mapa en el formato de texto de XKB (el de `xkbcomp` y de Wayland).
    pub fn from_string(text: String, compose: Compose<'_>) -> Result<Self, String> {
        let context = context();
        let keymap = xkb::Keymap::new_from_string(
            &context,
            text,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| "libxkbcommon no pudo compilar el mapa de teclado".to_owned())?;
        Ok(Self::build(&context, keymap, compose))
    }

    /// Compila el mapa de un modelo, distribución, variante y opciones (RMLVO, reglas `evdev`) con
    /// los datos de XKB del sistema, sin mirar las variables `XKB_DEFAULT_*`.
    pub fn from_names(
        model: &str,
        layout: &str,
        variant: &str,
        options: &str,
        compose: Compose<'_>,
    ) -> Result<Self, String> {
        let context = context();
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "evdev",
            model,
            layout,
            variant,
            Some(options.to_owned()),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| format!("libxkbcommon no pudo compilar la distribución «{layout}»"))?;
        Ok(Self::build(&context, keymap, compose))
    }

    fn build(context: &xkb::Context, keymap: xkb::Keymap, compose: Compose<'_>) -> Self {
        let table = compose_table(context, compose);
        let mut keys = Vec::new();
        keymap.key_for_each(|_, key| keys.push(key));
        let groups = (0..keymap.num_layouts())
            .map(|layout| group(&keymap, &keys, layout, table.as_ref()))
            .collect();
        Self {
            keymap: OwnedKeymap(keymap),
            groups,
        }
    }

    /// Distribuciones del mapa.
    pub fn groups(&self) -> u32 {
        self.groups.len() as u32
    }

    /// La máscara del modificador `name` (`"Lock"`, `"Mod2"`…), o `0` si el mapa no lo tiene.
    pub fn mod_mask(&self, name: &str) -> xkb::ModMask {
        let index = self.keymap.0.mod_get_index(name);
        if index == xkb::MOD_INVALID {
            0
        } else {
            1 << index
        }
    }

    fn group(&self, group: u32) -> Option<&Group> {
        // Un grupo fuera de rango da la vuelta, como en XKB por defecto.
        let count = self.groups.len();
        (count > 0).then(|| &self.groups[group as usize % count])
    }

    /// Cómo teclear `ch` con el teclado en `state` y las teclas que `held` dice que tienen
    /// pulsadas los invitados (que no se tocan: ni la tecla del carácter puede ser una de ellas,
    /// ni se suelta ningún modificador), si se puede.
    pub fn lookup(
        &self,
        ch: char,
        state: KeyboardState,
        held: impl Fn(u16) -> bool,
    ) -> Option<Typing> {
        let group = self.group(state.group)?;
        let layout = state.group % self.groups();
        let depressed = group
            .modifiers
            .iter()
            .filter(|(key, _)| held(*key))
            .fold(0, |mask, (_, m)| mask | m);
        let search = Search {
            keymap: &self.keymap.0,
            group,
            layout,
            depressed,
            locked: state.locked,
            shift_held: group.shift.iter().any(|key| held(*key)),
            level3_held: group.level3.iter().any(|key| held(*key)),
            held: &held,
        };
        let keysym = xkb::utf32_to_keysym(u32::from(ch)).raw();
        let direct = UNICODE_KEYSYM | u32::from(ch);
        let typed = [keysym, direct]
            .into_iter()
            .filter(|sym| *sym != 0)
            .filter_map(|sym| search.stroke(sym, Some(ch), state.latched))
            .min_by_key(Stroke::rank)
            .map(Typing::one);
        typed.or_else(|| {
            group
                .composed
                .get(&ch)?
                .iter()
                .filter_map(|&(dead, base)| {
                    let dead = search.stroke(dead, None, state.latched)?;
                    // Lo enganchado se gasta con la primera pulsación.
                    let base_ch = char::from_u32(xkb::keysym_to_utf32(base.into()));
                    let base = search.stroke(base, base_ch, 0)?;
                    Some(Typing::two(dead, base))
                })
                .min_by_key(Typing::rank)
        })
    }
}

/// Una búsqueda de [`TextKeymap::lookup`].
struct Search<'a> {
    keymap: &'a xkb::Keymap,
    group: &'a Group,
    layout: u32,
    /// Lo que ponen los modificadores que tienen pulsados los invitados.
    depressed: xkb::ModMask,
    locked: xkb::ModMask,
    shift_held: bool,
    level3_held: bool,
    held: &'a dyn Fn(u16) -> bool,
}

impl Search<'_> {
    /// La mejor pulsación que da `sym` (y `ch`, si se pide: así Control pulsado, que cambia el
    /// carácter, no vale) con el estado de la búsqueda y lo enganchado en `latched`.
    fn stroke(&self, sym: u32, ch: Option<char>, latched: xkb::ModMask) -> Option<Stroke> {
        let keys = self.group.keys.get(&sym)?;
        let shift = (!self.shift_held).then(|| self.group.shift.first().copied());
        let level3 = (!self.level3_held).then(|| self.group.level3.first().copied());
        // Lo que se puede añadir: nada, o la tecla de cada modificador que no esté ya pulsado.
        let mut options = vec![(None, None)];
        if let Some(Some(shift)) = shift {
            options.push((Some(shift), None));
        }
        if let Some(Some(level3)) = level3 {
            options.push((None, Some(level3)));
            if let Some(Some(shift)) = shift {
                options.push((Some(shift), Some(level3)));
            }
        }
        let mut state = xkb::State::new(self.keymap);
        keys.iter()
            .filter(|key| !(self.held)(**key))
            .flat_map(|&key| {
                options
                    .iter()
                    .map(move |&(shift, level3)| Stroke { key, shift, level3 })
            })
            .filter(|stroke| {
                let mut mask = self.depressed;
                if stroke.shift.is_some() {
                    mask |= self.group.shift_mask;
                }
                if stroke.level3.is_some() {
                    mask |= self.group.level3_mask;
                }
                state.update_mask(mask, latched, self.locked, 0, 0, self.layout);
                let code = xkb::Keycode::new(u32::from(stroke.key) + EVDEV_OFFSET);
                state.key_get_one_sym(code).raw() == sym
                    && ch.is_none_or(|ch| state.key_get_utf32(code) == u32::from(ch))
            })
            .min_by_key(Stroke::rank)
    }
}

fn context() -> xkb::Context {
    let mut context = xkb::Context::new(xkb::CONTEXT_NO_ENVIRONMENT_NAMES);
    // Los avisos del compilador de XKB (keysyms que no conoce…) no son nuestros.
    context.set_log_level(xkb::LogLevel::Error);
    context
}

/// El *locale* de este proceso para la composición, como lo eligen las aplicaciones.
fn host_locale() -> OsString {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "C".into())
}

fn compose_table(context: &xkb::Context, compose: Compose<'_>) -> Option<compose::Table> {
    let from_locale = |locale: &std::ffi::OsStr| {
        compose::Table::new_from_locale(context, locale, compose::COMPILE_NO_FLAGS)
            .or_else(|()| {
                compose::Table::new_from_locale(context, "C".as_ref(), compose::COMPILE_NO_FLAGS)
            })
            .inspect_err(|()| {
                tracing::debug!(
                    ?locale,
                    "sin tabla de composición: las teclas muertas no se usan para el texto"
                );
            })
            .ok()
    };
    match compose {
        Compose::Host => from_locale(&host_locale()),
        Compose::Locale(locale) => from_locale(locale.as_ref()),
        Compose::Rules(rules) => compose::Table::new_from_buffer(
            context,
            rules,
            "C",
            compose::FORMAT_TEXT_V1,
            compose::COMPILE_NO_FLAGS,
        )
        .ok(),
        Compose::None => None,
    }
}

fn evdev_code(key: xkb::Keycode) -> Option<u16> {
    key.raw()
        .checked_sub(EVDEV_OFFSET)
        .and_then(|code| u16::try_from(code).ok())
}

/// La máscara que pone `key` pulsada sola en `layout`.
fn mask_of(keymap: &xkb::Keymap, key: xkb::Keycode, layout: u32) -> xkb::ModMask {
    let mut state = xkb::State::new(keymap);
    state.update_mask(0, 0, 0, 0, 0, layout);
    state.update_key(key, xkb::KeyDirection::Down);
    state.serialize_mods(xkb::STATE_MODS_DEPRESSED)
}

/// De `modifiers`, las teclas cuyo primer nivel en `layout` es uno de `wanted` y que ponen la
/// misma máscara que la preferida: las de `wanted` primero en su orden, y en cada una `preferred`
/// delante y el resto por código.
fn modifier_keys(
    keymap: &xkb::Keymap,
    modifiers: &[(u16, xkb::ModMask)],
    layout: u32,
    wanted: &[u32],
    preferred: u16,
) -> (Vec<u16>, xkb::ModMask) {
    let mut found: Vec<(usize, bool, u16, xkb::ModMask)> = modifiers
        .iter()
        .filter_map(|&(code, mask)| {
            let key = xkb::Keycode::new(u32::from(code) + EVDEV_OFFSET);
            let [sym] = keymap.key_get_syms_by_level(key, layout, 0) else {
                return None;
            };
            let rank = wanted.iter().position(|w| *w == sym.raw())?;
            Some((rank, code != preferred, code, mask))
        })
        .collect();
    found.sort_unstable();
    let mask = found.first().map_or(0, |(_, _, _, mask)| *mask);
    let codes = found
        .into_iter()
        .filter(|(_, _, _, m)| *m == mask)
        .map(|(_, _, code, _)| code)
        .collect();
    (codes, mask)
}

fn group(
    keymap: &xkb::Keymap,
    keys: &[xkb::Keycode],
    layout: u32,
    table: Option<&compose::Table>,
) -> Group {
    let modifiers: Vec<(u16, xkb::ModMask)> = keys
        .iter()
        .filter_map(|&key| {
            let code = evdev_code(key)?;
            let mask = mask_of(keymap, key, layout);
            (mask != 0).then_some((code, mask))
        })
        .collect();
    let (shift, shift_mask) = modifier_keys(
        keymap,
        &modifiers,
        layout,
        &[xkb::keysyms::KEY_Shift_L, xkb::keysyms::KEY_Shift_R],
        codes::KEY_LEFTSHIFT,
    );
    let (level3, level3_mask) = modifier_keys(
        keymap,
        &modifiers,
        layout,
        &[xkb::keysyms::KEY_ISO_Level3_Shift],
        codes::KEY_RIGHTALT,
    );
    let mut by_sym: HashMap<u32, Vec<u16>> = HashMap::new();
    for &key in keys {
        let Some(code) = evdev_code(key) else {
            continue;
        };
        if modifiers.iter().any(|(m, _)| *m == code) {
            continue;
        }
        for level in 0..keymap.num_levels_for_key(key, layout) {
            if let [sym] = keymap.key_get_syms_by_level(key, layout, level) {
                let codes = by_sym.entry(sym.raw()).or_default();
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
        }
    }
    for codes in by_sym.values_mut() {
        codes.sort_unstable_by_key(|code| {
            (u32::from(*code) + EVDEV_OFFSET > MAX_X11_KEYCODE, *code)
        });
    }
    let composed = table
        .map(|table| composed(table, &by_sym))
        .unwrap_or_default();
    Group {
        keys: by_sym,
        composed,
        modifiers,
        shift,
        shift_mask,
        level3,
        level3_mask,
    }
}

/// Lo que la tabla de composición da con cada tecla muerta de la distribución seguida de cada
/// carácter de la distribución, comprobado con un `xkb_compose_state`.
fn composed(
    table: &compose::Table,
    keys: &HashMap<u32, Vec<u16>>,
) -> HashMap<char, Vec<(u32, u32)>> {
    let dead: BTreeSet<u32> = keys
        .keys()
        .copied()
        .filter(|sym| DEAD_KEYSYMS.contains(sym))
        .collect();
    let bases: BTreeSet<u32> = keys
        .keys()
        .copied()
        .filter(|sym| !DEAD_KEYSYMS.contains(sym) && xkb::keysym_to_utf32((*sym).into()) != 0)
        .collect();
    let mut state = compose::State::new(table, compose::STATE_NO_FLAGS);
    let mut out: HashMap<char, Vec<(u32, u32)>> = HashMap::new();
    for &dead in &dead {
        for &base in &bases {
            state.reset();
            state.feed(dead.into());
            state.feed(base.into());
            if state.status() != compose::Status::Composed {
                continue;
            }
            let Some(text) = state.utf8() else {
                continue;
            };
            let mut chars = text.chars();
            if let (Some(ch), None) = (chars.next(), chars.next()) {
                out.entry(ch).or_default().push((dead, base));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KEY_SEMICOLON` (la Ñ en la distribución española).
    const KEY_SEMICOLON: u16 = 39;
    /// `KEY_APOSTROPHE` (las tildes en la distribución española).
    const KEY_APOSTROPHE: u16 = 40;
    /// `KEY_2`.
    const KEY_2: u16 = 3;
    /// `KEY_EQUAL`.
    const KEY_EQUAL: u16 = 13;
    /// `KEY_U`.
    const KEY_U: u16 = 22;

    /// Unas reglas de composición como las de los ficheros `Compose` del sistema.
    const RULES: &str = "\
<dead_acute> <a> : \"á\" aacute\n\
<dead_acute> <A> : \"Á\" Aacute\n\
<dead_acute> <e> : \"é\" eacute\n\
<dead_diaeresis> <u> : \"ü\" udiaeresis\n\
<dead_diaeresis> <U> : \"Ü\" Udiaeresis\n";

    fn layout(name: &str) -> TextKeymap {
        TextKeymap::from_names("pc105", name, "", "", Compose::Rules(RULES))
            .expect("distribución de XKB")
    }

    fn idle() -> KeyboardState {
        KeyboardState::default()
    }

    fn nothing_held(_: u16) -> bool {
        false
    }

    fn one(typing: Option<Typing>) -> Stroke {
        let typing = typing.expect("se puede teclear");
        assert_eq!(typing.strokes().len(), 1, "{typing:?}");
        typing.strokes()[0]
    }

    fn plain(key: u16) -> Stroke {
        Stroke {
            key,
            shift: None,
            level3: None,
        }
    }

    fn shifted(key: u16) -> Stroke {
        Stroke {
            shift: Some(codes::KEY_LEFTSHIFT),
            ..plain(key)
        }
    }

    #[test]
    fn spanish_has_its_own_key_for_enye() {
        let es = layout("es");
        assert_eq!(
            one(es.lookup('ñ', idle(), nothing_held)),
            plain(KEY_SEMICOLON)
        );
        assert_eq!(
            one(es.lookup('Ñ', idle(), nothing_held)),
            shifted(KEY_SEMICOLON)
        );
    }

    #[test]
    fn spanish_types_the_at_sign_and_the_euro_with_altgr() {
        let es = layout("es");
        let altgr = |key| Stroke {
            level3: Some(codes::KEY_RIGHTALT),
            ..plain(key)
        };
        assert_eq!(one(es.lookup('@', idle(), nothing_held)), altgr(KEY_2));
        // La «€» de la E (no la tecla `KEY_EURO`, por encima de 255).
        assert_eq!(
            one(es.lookup('€', idle(), nothing_held)),
            altgr(codes::KEY_E)
        );
        // Con AltGr ya pulsado por un invitado, no se vuelve a pulsar.
        let held = |key| key == codes::KEY_RIGHTALT;
        assert_eq!(one(es.lookup('@', idle(), held)), plain(KEY_2));
    }

    #[test]
    fn us_has_no_enye_but_has_plain_letters() {
        let us = layout("us");
        assert_eq!(us.lookup('ñ', idle(), nothing_held), None);
        assert_eq!(us.lookup('😀', idle(), nothing_held), None);
        assert_eq!(
            one(us.lookup('a', idle(), nothing_held)),
            plain(codes::KEY_A)
        );
        assert_eq!(
            one(us.lookup('A', idle(), nothing_held)),
            shifted(codes::KEY_A)
        );
        // `+` va con Mayúsculas en su tecla, no por el teclado numérico (otro keysym).
        assert_eq!(
            one(us.lookup('+', idle(), nothing_held)),
            shifted(KEY_EQUAL)
        );
    }

    /// Las vocales con tilde y diéresis se componen con la tecla muerta de la distribución y la
    /// vocal, cada una con sus modificadores; donde no hay tecla muerta, no.
    #[test]
    fn accents_are_typed_with_the_dead_key() {
        let es = layout("es");
        let strokes = |ch| {
            es.lookup(ch, idle(), nothing_held)
                .map(|typing| typing.strokes().to_vec())
        };
        assert_eq!(
            strokes('á'),
            Some(vec![plain(KEY_APOSTROPHE), plain(codes::KEY_A)])
        );
        assert_eq!(
            strokes('Á'),
            Some(vec![plain(KEY_APOSTROPHE), shifted(codes::KEY_A)])
        );
        assert_eq!(
            strokes('ü'),
            Some(vec![shifted(KEY_APOSTROPHE), plain(KEY_U)])
        );
        assert_eq!(
            strokes('Ü'),
            Some(vec![shifted(KEY_APOSTROPHE), shifted(KEY_U)])
        );
        // En la latinoamericana, la tecla muerta es otra.
        let latam = layout("latam");
        let typing = latam.lookup('é', idle(), nothing_held).expect("é en latam");
        assert_eq!(typing.strokes().len(), 2);
        assert_eq!(typing.strokes()[1], plain(codes::KEY_E));
        // Sin regla de composición, no.
        assert_eq!(strokes('ó'), None);
        assert_eq!(layout("us").lookup('á', idle(), nothing_held), None);
        let bare = TextKeymap::from_names("pc105", "es", "", "", Compose::None).expect("mapa");
        assert_eq!(bare.lookup('á', idle(), nothing_held), None);
    }

    /// Con Bloq Mayús activo en el anfitrión, la ñ pide Mayúsculas y la Ñ no.
    #[test]
    fn caps_lock_is_taken_into_account() {
        let es = layout("es");
        let caps = KeyboardState {
            locked: es.mod_mask("Lock"),
            ..idle()
        };
        assert_ne!(caps.locked, 0);
        assert_eq!(
            one(es.lookup('ñ', caps, nothing_held)),
            shifted(KEY_SEMICOLON)
        );
        assert_eq!(
            one(es.lookup('Ñ', caps, nothing_held)),
            plain(KEY_SEMICOLON)
        );
        assert_eq!(
            one(es.lookup('a', caps, nothing_held)),
            shifted(codes::KEY_A)
        );
        // Los números no cambian con Bloq Mayús.
        assert_eq!(one(es.lookup('2', caps, nothing_held)), plain(KEY_2));
        // La á: tecla muerta y la a con Mayúsculas (que con Bloq Mayús da la minúscula).
        assert_eq!(
            es.lookup('á', caps, nothing_held)
                .map(|typing| typing.strokes().to_vec()),
            Some(vec![plain(KEY_APOSTROPHE), shifted(codes::KEY_A)])
        );
    }

    /// Lo que pulsan los invitados no se suelta: con Mayúsculas pulsada, la Ñ sale sin pulsar
    /// nada más y la ñ no se puede teclear; con Control, nada (cambiaría el carácter); y la tecla
    /// del carácter no puede estar pulsada.
    #[test]
    fn modifiers_held_by_guests_are_respected() {
        let es = layout("es");
        let shift = |key| key == codes::KEY_LEFTSHIFT;
        assert_eq!(one(es.lookup('Ñ', idle(), shift)), plain(KEY_SEMICOLON));
        assert_eq!(es.lookup('ñ', idle(), shift), None);
        let right_shift = |key| key == codes::KEY_RIGHTSHIFT;
        assert_eq!(
            one(es.lookup('Ñ', idle(), right_shift)),
            plain(KEY_SEMICOLON)
        );
        let ctrl = |key| key == codes::KEY_LEFTCTRL;
        assert_eq!(es.lookup('a', idle(), ctrl), None);
        let enye = |key| key == KEY_SEMICOLON;
        assert_eq!(es.lookup('ñ', idle(), enye), None);
    }

    #[test]
    fn groups_are_looked_up_separately() {
        let both = layout("us,es");
        assert_eq!(both.groups(), 2);
        assert_eq!(both.lookup('ñ', idle(), nothing_held), None);
        let second = KeyboardState { group: 1, ..idle() };
        assert_eq!(
            one(both.lookup('ñ', second, nothing_held)),
            plain(KEY_SEMICOLON)
        );
        // Fuera de rango da la vuelta.
        let wrapped = KeyboardState { group: 3, ..idle() };
        assert_eq!(
            one(both.lookup('ñ', wrapped, nothing_held)),
            plain(KEY_SEMICOLON)
        );
    }

    #[test]
    fn the_text_format_round_trips() {
        let context = context();
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            "es",
            "",
            Some(String::new()),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("mapa");
        let text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        let parsed =
            TextKeymap::from_string(text, Compose::Rules(RULES)).expect("el mapa en texto");
        assert_eq!(
            one(parsed.lookup('ñ', idle(), nothing_held)),
            plain(KEY_SEMICOLON)
        );
        assert!(TextKeymap::from_string("no es un mapa".to_owned(), Compose::None).is_err());
    }

    /// Con la tabla de composición del sistema (la de los ficheros `Compose` de libX11, si están
    /// instalados), la á de la distribución española sale con su tecla muerta.
    #[test]
    fn the_system_compose_table_composes_accents() {
        if !std::path::Path::new("/usr/share/X11/locale/compose.dir").exists() {
            return;
        }
        let es = TextKeymap::from_names("pc105", "es", "", "", Compose::Locale("en_US.UTF-8"))
            .expect("mapa");
        assert_eq!(
            es.lookup('é', idle(), nothing_held)
                .map(|typing| typing.strokes().to_vec()),
            Some(vec![plain(KEY_APOSTROPHE), plain(codes::KEY_E)])
        );
    }
}
