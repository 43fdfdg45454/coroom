//! Inyección de la entrada en el anfitrión: el trait [`InputSink`] y sus backends.
//!
//! La sesión (`coroom-session`) ya ha comprobado permisos, ranura y pausa antes de llamar a
//! [`InputSink::inject`]; el sumidero solo traduce y escribe. Varios invitados comparten el teclado
//! y el ratón del sistema, así que las pulsaciones se cuentan por invitado ([`PressedKeys`]): una
//! tecla se suelta en el sistema cuando la suelta el último que la tenía pulsada, y
//! [`InputSink::release_all`] suelta solo lo de ese invitado. Los mandos son un dispositivo por
//! ranura ([`InputSink::assign_gamepad`] / [`InputSink::release_gamepad`]). Un mando **tal cual**
//! ([`InputSink::connect_raw`]) sustituye al de la ranura por uno idéntico al del invitado (UHID o
//! uinput con su identidad en Linux); el sumidero que no sepa, lo rechaza y la sesión sigue con el
//! convertido.
//!
//! Backends: [`linux`] (feature `linux-portal`: libei por el portal `RemoteDesktop` y uinput),
//! [`windows`] (feature `windows-sendinput`: `SendInput`; los mandos por el driver propio de
//! [`windows_vhid`], feature `windows-vhid`, o como teclas con [`GamepadToKeys`]),
//! [`RecordingSink`] para los tests y [`NullSink`] cuando no hay ninguno.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::keymap::evdev as codes;
use crate::{GamepadAxis, GamepadButton, GuestId, InputError, InputEvent, MouseButton};
use coroom_proto::input::{GamepadDevice, GamepadTransport};

#[cfg(all(target_os = "linux", feature = "linux-portal"))]
pub mod linux;
#[cfg(all(target_os = "linux", feature = "linux-portal"))]
pub mod uhid;
#[cfg(all(target_os = "windows", feature = "windows-sendinput"))]
pub mod windows;
// El formato del driver es independiente de la plataforma (y se prueba en todas); solo el acceso
// al driver es de Windows.
#[cfg(feature = "windows-vhid")]
pub mod windows_vhid;

/// Destino de la entrada en el anfitrión.
///
/// Es `Send` porque vive en el hilo `inject` de la sesión.
pub trait InputSink: Send {
    /// Inyecta un evento en nombre de `guest` (ya autorizado por la sesión).
    fn inject(&mut self, event: &InputEvent, guest: GuestId) -> Result<(), InputError>;

    /// Inyecta varios eventos de `guest` de una vez, en orden. Los sumideros que pueden los
    /// escriben juntos (una trama de uinput o de EI, una llamada a `SendInput`): un paquete con
    /// `LeftX` y `LeftY` llega al juego como una sola muestra del stick, no como dos a medias.
    /// Devuelve cuántos se aplicaron y deja en `errors` por qué fallaron los demás (uno por evento
    /// o uno por todo el grupo). Por defecto, uno a uno con [`InputSink::inject`].
    fn inject_batch(
        &mut self,
        events: &[InputEvent],
        guest: GuestId,
        errors: &mut Vec<InputError>,
    ) -> usize {
        let mut applied = 0;
        for event in events {
            match self.inject(event, guest) {
                Ok(()) => applied += 1,
                Err(err) => errors.push(err),
            }
        }
        applied
    }

    /// Trabajo periódico del sumidero (la repetición automática de teclas donde el sistema no la
    /// hace, [`KeyRepeat`]): la sesión lo llama en cada vuelta de su hilo y vuelve a llamarlo, como
    /// tarde, en el instante que devuelva. Por defecto no hay nada.
    fn tick(&mut self, now: Instant) -> Option<Instant> {
        let _ = now;
        None
    }

    /// Suelta todo lo que `guest` tenga pulsado (teclas, botones y su mando, que vuelve al reposo).
    fn release_all(&mut self, guest: GuestId) -> Result<(), InputError>;

    /// Crea el mando virtual de la ranura `slot` (desde 1).
    fn assign_gamepad(&mut self, slot: u8) -> Result<(), InputError>;

    /// Destruye el mando virtual de `slot`.
    fn release_gamepad(&mut self, slot: u8) -> Result<(), InputError>;

    /// Prepara el teclado y el ratón (abre el portal, que puede pedir permiso al usuario) antes
    /// del primer evento. La sesión la llama al conceder teclado y ratón a algún invitado.
    fn prepare(&mut self) {}

    /// La captura cambió de fuente con la sala abierta y su sesión nueva del portal trae otra
    /// conexión EIS (Linux): el teclado y el ratón pasan a ella. Donde no hay portal, nada.
    fn replace_eis(&mut self, eis: EisProvider) {
        let _ = eis;
    }

    /// Nombre del backend para los avisos (`libei + uinput`, `SendInput`…).
    fn describe(&self) -> String;

    /// Sustituye el mando de `slot` por uno idéntico al físico del invitado (`device`, ya
    /// validado por la sesión): con su descriptor si es HID, con su identidad si es XInput. Por
    /// defecto no se puede, y la ranura sigue con su mando convertido.
    fn connect_raw(&mut self, slot: u8, device: &GamepadDevice) -> Result<(), InputError> {
        let _ = (slot, device);
        Err(InputError::Unsupported(
            "este anfitrión no recrea mandos tal cual".to_owned(),
        ))
    }

    /// Un informe de entrada HID tal cual para el mando de `slot`.
    fn raw_report(&mut self, slot: u8, data: &[u8]) -> Result<(), InputError> {
        let _ = data;
        Err(InputError::NoSuchSlot(slot))
    }

    /// Quita el mando tal cual de `slot` y vuelve a poner el convertido si la ranura sigue
    /// asignada.
    fn disconnect_raw(&mut self, slot: u8) -> Result<(), InputError> {
        let _ = slot;
        Ok(())
    }

    /// Informes de salida (vibración, luces) que el driver del anfitrión mandó a los mandos tal
    /// cual desde la última llamada: ranura y bytes.
    fn take_outputs(&mut self) -> Vec<(u8, Vec<u8>)> {
        Vec::new()
    }
}

/// Conexión EIS (libei) ya abierta por quien tenga la sesión del portal `RemoteDesktop` (la
/// captura de pantalla, que la abre después de su sesión `ScreenCast`).
#[derive(Debug)]
pub struct EisConnection {
    /// Socket Unix del protocolo EI.
    #[cfg(unix)]
    pub fd: std::os::fd::OwnedFd,
    /// Qué se transmite, para llevar la posición absoluta del invitado al sitio correcto.
    pub area: PointerArea,
}

/// Zona del escritorio que ve el invitado, en coordenadas lógicas del compositor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PointerArea {
    /// Sin datos: la posición absoluta va a la primera región del dispositivo.
    #[default]
    Unknown,
    /// Un monitor: la región de EI con su `mapping_id` o, si el portal no lo da, la que empieza
    /// en `(x, y)`.
    Monitor {
        /// Esquina superior izquierda.
        x: i32,
        /// Esquina superior izquierda.
        y: i32,
        /// Identificador del flujo que repite la región de EI del monitor.
        mapping_id: Option<String>,
    },
    /// Una ventana: Wayland no dice dónde está, así que la posición absoluta se convierte en
    /// movimiento relativo escalado a su tamaño.
    Window {
        /// Ancho lógico.
        width: u32,
        /// Alto lógico.
        height: u32,
    },
}

/// Quien da la conexión EIS cuando hace falta (bloquea hasta tenerla).
pub type EisProvider = Box<dyn FnOnce() -> Result<EisConnection, String> + Send>;

/// Opciones del sumidero de la plataforma.
#[derive(Default)]
pub struct SinkOptions {
    /// Windows: convierte los mandos en teclas con el perfil fijo de [`GamepadToKeys`] en vez de
    /// usar el driver de mando virtual (por ejemplo, si no está instalado). En Linux se ignora:
    /// hay uinput.
    pub gamepad_to_keys: bool,
    /// Linux: conexión EIS de la sesión del portal compartida con la captura. Sin ella, el
    /// sumidero abre su propia sesión `RemoteDesktop` (solo dispositivos) al necesitarla.
    pub eis: Option<EisProvider>,
}

/// Sumidero de la plataforma, perezoso: no abre el portal ni crea dispositivos hasta que hace falta.
pub fn platform_sink(options: SinkOptions) -> Box<dyn InputSink> {
    #[cfg(all(target_os = "linux", feature = "linux-portal"))]
    {
        let _ = options.gamepad_to_keys;
        Box::new(linux::LinuxSink::new(options.eis))
    }
    #[cfg(all(target_os = "windows", feature = "windows-sendinput"))]
    {
        let _ = options.eis;
        let sink = windows::SendInputSink::new();
        if options.gamepad_to_keys {
            Box::new(GamepadToKeys::new(sink))
        } else {
            Box::new(sink)
        }
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "linux-portal"),
        all(target_os = "windows", feature = "windows-sendinput")
    )))]
    {
        let _ = options;
        Box::new(NullSink::new(
            "no hay backend de inyección compilado para esta plataforma",
        ))
    }
}

/// Estado de la inyección en este equipo para `coroom doctor` (sin abrir sesiones ni pedir
/// permisos).
pub fn status() -> Vec<String> {
    #[cfg(all(target_os = "linux", feature = "linux-portal"))]
    {
        linux::status()
    }
    #[cfg(all(target_os = "windows", feature = "windows-sendinput"))]
    {
        windows::status()
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "linux-portal"),
        all(target_os = "windows", feature = "windows-sendinput")
    )))]
    {
        vec!["sin backend de inyección compilado para esta plataforma".to_owned()]
    }
}

/// Pulsaciones de teclas y botones del ratón por invitado.
#[derive(Debug, Default)]
pub struct PressedKeys {
    keys: BTreeMap<u16, BTreeSet<GuestId>>,
    buttons: BTreeMap<MouseButton, BTreeSet<GuestId>>,
}

fn edge<K: Ord + Copy>(
    map: &mut BTreeMap<K, BTreeSet<GuestId>>,
    key: K,
    guest: GuestId,
    pressed: bool,
) -> bool {
    if pressed {
        let holders = map.entry(key).or_default();
        let first = holders.is_empty();
        holders.insert(guest);
        first
    } else {
        let Some(holders) = map.get_mut(&key) else {
            return false;
        };
        if !holders.remove(&guest) {
            return false;
        }
        let last = holders.is_empty();
        if last {
            map.remove(&key);
        }
        last
    }
}

impl PressedKeys {
    /// Anota una tecla; `true` si hay que escribirla en el sistema (primera pulsación o última
    /// liberación).
    pub fn key(&mut self, guest: GuestId, code: u16, pressed: bool) -> bool {
        edge(&mut self.keys, code, guest, pressed)
    }

    /// Como [`PressedKeys::key`] para un botón del ratón.
    pub fn button(&mut self, guest: GuestId, button: MouseButton, pressed: bool) -> bool {
        edge(&mut self.buttons, button, guest, pressed)
    }

    /// Quita las pulsaciones de `guest`; devuelve las teclas y botones que ya nadie tiene
    /// pulsados (los que hay que soltar en el sistema).
    pub fn release_guest(&mut self, guest: GuestId) -> (Vec<u16>, Vec<MouseButton>) {
        let keys: Vec<u16> = self
            .keys
            .iter()
            .filter(|(_, holders)| holders.contains(&guest))
            .map(|(code, _)| *code)
            .collect();
        let buttons: Vec<MouseButton> = self
            .buttons
            .iter()
            .filter(|(_, holders)| holders.contains(&guest))
            .map(|(button, _)| *button)
            .collect();
        (
            keys.into_iter()
                .filter(|code| self.key(guest, *code, false))
                .collect(),
            buttons
                .into_iter()
                .filter(|button| self.button(guest, *button, false))
                .collect(),
        )
    }
}

/// Repetición automática de teclas (*typematic*) para los sumideros cuyo sistema no la hace con
/// la entrada inyectada (Windows: `SendInput` escribe una pulsación y no vuelve a repetirla; los
/// dispositivos de EI y uinput de Linux la hacen solos). Como un teclado: se repite la **última**
/// tecla pulsada, tras el retardo inicial y luego a la cadencia; soltarla para la repetición
/// aunque sigan otras pulsadas, y pulsar otra pasa la repetición a esa. Los modificadores y los
/// bloqueos no se repiten ([`KeyRepeat::repeats`]). El retardo y la cadencia los da el sistema
/// ([`KeyRepeat::from_windows_settings`]), no una cifra fija.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRepeat {
    delay: Duration,
    interval: Duration,
    /// La tecla que se repite y cuándo toca la siguiente repetición.
    current: Option<(u16, Instant)>,
}

impl KeyRepeat {
    /// Repetición con `delay` hasta la primera y `interval` entre las siguientes.
    pub fn new(delay: Duration, interval: Duration) -> Self {
        Self {
            delay,
            interval: interval.max(Duration::from_millis(1)),
            current: None,
        }
    }

    /// Con los valores de Windows: `SPI_GETKEYBOARDDELAY` (`0..=3`, 250 ms × (n + 1)) y
    /// `SPI_GETKEYBOARDSPEED` (`0..=31`, de unas 2,5 a unas 30 repeticiones por segundo, lineal).
    pub fn from_windows_settings(delay: u32, speed: u32) -> Self {
        let delay_ms = 250 * (u64::from(delay.min(3)) + 1);
        let per_second = 2.5 + 27.5 * f64::from(speed.min(31)) / 31.0;
        Self::new(
            Duration::from_millis(delay_ms),
            Duration::from_secs_f64(1.0 / per_second),
        )
    }

    /// Cambia el retardo y la cadencia (el sistema los cambió) sin tocar la tecla en curso.
    pub fn set_timing(&mut self, delay: Duration, interval: Duration) {
        self.delay = delay;
        self.interval = interval.max(Duration::from_millis(1));
    }

    /// Retardo hasta la primera repetición y cadencia entre las siguientes.
    pub fn timing(&self) -> (Duration, Duration) {
        (self.delay, self.interval)
    }

    /// Si la tecla evdev `code` se repite al mantenerla pulsada: todas salvo los modificadores
    /// (Mayús, Ctrl, Alt, Meta) y los bloqueos (Bloq Mayús, Bloq Num, Bloq Despl).
    pub fn repeats(code: u16) -> bool {
        !matches!(
            code,
            codes::KEY_LEFTSHIFT
                | codes::KEY_RIGHTSHIFT
                | codes::KEY_LEFTCTRL
                | codes::KEY_RIGHTCTRL
                | codes::KEY_LEFTALT
                | codes::KEY_RIGHTALT
                | codes::KEY_LEFTMETA
                | codes::KEY_RIGHTMETA
                | codes::KEY_CAPSLOCK
                | codes::KEY_NUMLOCK
                | codes::KEY_SCROLLLOCK
        )
    }

    /// La tecla `code` acaba de escribirse pulsada en el sistema en `now`. Como en un teclado
    /// real, la última pulsada es la que se repite, y pulsar un modificador o un bloqueo corta
    /// la repetición en curso (A mantenida y Mayús después no sigue repitiendo Mayús+A).
    pub fn pressed(&mut self, code: u16, now: Instant) {
        self.current = Self::repeats(code).then_some((code, now + self.delay));
    }

    /// La tecla `code` acaba de soltarse en el sistema.
    pub fn released(&mut self, code: u16) {
        if self.current.is_some_and(|(current, _)| current == code) {
            self.current = None;
        }
    }

    /// Para la repetición (todo se soltó o la conexión cambió).
    pub fn clear(&mut self) {
        self.current = None;
    }

    /// La tecla que toca repetir en `now`, si alguna, y la cita siguiente. Como mucho una
    /// repetición por llamada: si el hilo llegó tarde, la siguiente se cuenta desde `now` (no se
    /// acumulan repeticiones atrasadas).
    pub fn due(&mut self, now: Instant) -> Option<u16> {
        let (code, at) = self.current.as_mut()?;
        if now < *at {
            return None;
        }
        *at = (*at + self.interval).max(now + self.interval / 2);
        Some(*code)
    }

    /// Cuándo toca la siguiente repetición, si hay alguna tecla repitiéndose.
    pub fn next(&self) -> Option<Instant> {
        self.current.map(|(_, at)| at)
    }
}

/// Estado de los mandos virtuales: quién usa cada ranura, botones pulsados y ejes fuera de reposo.
#[derive(Debug, Default)]
pub struct PadTracker {
    pads: BTreeMap<u8, PadState>,
}

#[derive(Debug, Default)]
struct PadState {
    owner: Option<GuestId>,
    buttons: BTreeSet<GamepadButton>,
    axes: BTreeMap<GamepadAxis, i16>,
}

impl PadTracker {
    /// Anota un evento de mando de `guest`.
    pub fn note(&mut self, guest: GuestId, event: &InputEvent) {
        let (slot, pad) = match event {
            InputEvent::GamepadButton { slot, .. }
            | InputEvent::GamepadAxis { slot, .. }
            | InputEvent::GamepadConnected { slot, .. }
            | InputEvent::GamepadDisconnected { slot } => {
                (*slot, self.pads.entry(*slot).or_default())
            }
            _ => return,
        };
        let _ = slot;
        pad.owner = Some(guest);
        match event {
            InputEvent::GamepadButton {
                button, pressed, ..
            } => {
                if *pressed {
                    pad.buttons.insert(*button);
                } else {
                    pad.buttons.remove(button);
                }
            }
            InputEvent::GamepadAxis { axis, value, .. } => {
                if *value == 0 {
                    pad.axes.remove(axis);
                } else {
                    pad.axes.insert(*axis, *value);
                }
            }
            _ => {}
        }
    }

    /// Eventos que devuelven al reposo los mandos que usa `guest` (o la ranura `slot`, si se da).
    pub fn neutral_events(&mut self, guest: Option<GuestId>, slot: Option<u8>) -> Vec<InputEvent> {
        let mut out = Vec::new();
        for (pad_slot, pad) in &mut self.pads {
            let matches = guest.is_none_or(|guest| pad.owner == Some(guest))
                && slot.is_none_or(|slot| slot == *pad_slot);
            if !matches {
                continue;
            }
            for button in std::mem::take(&mut pad.buttons) {
                out.push(InputEvent::GamepadButton {
                    slot: *pad_slot,
                    button,
                    pressed: false,
                });
            }
            for axis in std::mem::take(&mut pad.axes).into_keys() {
                out.push(InputEvent::GamepadAxis {
                    slot: *pad_slot,
                    axis,
                    value: 0,
                });
            }
        }
        out
    }

    /// Olvida la ranura (al destruir su mando).
    pub fn forget(&mut self, slot: u8) {
        self.pads.remove(&slot);
    }
}

/// Sumidero que descarta todo y explica por qué.
#[derive(Debug, Clone)]
pub struct NullSink {
    reason: String,
}

impl NullSink {
    /// Sumidero nulo con el motivo que se dará en cada error.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl InputSink for NullSink {
    fn inject(&mut self, _event: &InputEvent, _guest: GuestId) -> Result<(), InputError> {
        Err(InputError::Unsupported(self.reason.clone()))
    }

    fn release_all(&mut self, _guest: GuestId) -> Result<(), InputError> {
        Ok(())
    }

    fn assign_gamepad(&mut self, _slot: u8) -> Result<(), InputError> {
        Err(InputError::GamepadUnavailable(self.reason.clone()))
    }

    fn release_gamepad(&mut self, _slot: u8) -> Result<(), InputError> {
        Ok(())
    }

    fn describe(&self) -> String {
        format!("sin inyección ({})", self.reason)
    }
}

/// Lo que recibió un [`RecordingSink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// Un evento inyectado.
    Event {
        /// Invitado.
        guest: GuestId,
        /// Evento.
        event: InputEvent,
    },
    /// [`InputSink::release_all`].
    ReleaseAll {
        /// Invitado.
        guest: GuestId,
    },
    /// [`InputSink::assign_gamepad`].
    GamepadAssigned {
        /// Ranura.
        slot: u8,
    },
    /// [`InputSink::release_gamepad`].
    GamepadReleased {
        /// Ranura.
        slot: u8,
    },
    /// [`InputSink::connect_raw`].
    RawConnected {
        /// Ranura.
        slot: u8,
        /// VID.
        vendor: u16,
        /// PID.
        product: u16,
        /// HID (si no, XInput).
        hid: bool,
    },
    /// [`InputSink::raw_report`].
    RawReport {
        /// Ranura.
        slot: u8,
        /// Bytes.
        len: usize,
    },
    /// [`InputSink::disconnect_raw`].
    RawDisconnected {
        /// Ranura.
        slot: u8,
    },
}

#[derive(Debug, Default)]
struct RecordingState {
    log: Vec<Recorded>,
    slots: BTreeSet<u8>,
    raw: BTreeSet<u8>,
    raw_unsupported: bool,
    outputs: Vec<(u8, Vec<u8>)>,
    /// Tamaño de cada grupo recibido por [`InputSink::inject_batch`].
    batches: Vec<usize>,
}

/// Sumidero de prueba (público para los tests de las demás crates): guarda todo lo que recibe con
/// su invitado. Se clona para quedarse con una copia mientras la sesión usa otra.
#[derive(Debug, Clone, Default)]
pub struct RecordingSink {
    state: Arc<Mutex<RecordingState>>,
}

impl RecordingSink {
    /// Sumidero vacío.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, RecordingState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Todo lo recibido, en orden.
    pub fn log(&self) -> Vec<Recorded> {
        self.lock().log.clone()
    }

    /// Entradas recibidas hasta ahora.
    pub fn len(&self) -> usize {
        self.lock().log.len()
    }

    /// Indica si no ha recibido nada.
    pub fn is_empty(&self) -> bool {
        self.lock().log.is_empty()
    }

    /// Eventos inyectados para `guest`, en orden.
    pub fn events_of(&self, guest: GuestId) -> Vec<InputEvent> {
        self.lock()
            .log
            .iter()
            .filter_map(|entry| match entry {
                Recorded::Event { guest: g, event } if *g == guest => Some(*event),
                _ => None,
            })
            .collect()
    }

    /// Ranuras con mando creado ahora.
    pub fn assigned_slots(&self) -> Vec<u8> {
        self.lock().slots.iter().copied().collect()
    }

    /// Ranuras con mando tal cual ahora.
    pub fn raw_slots(&self) -> Vec<u8> {
        self.lock().raw.iter().copied().collect()
    }

    /// Tamaño de cada grupo recibido por [`InputSink::inject_batch`], en orden (los eventos del
    /// grupo están también en el registro, uno a uno).
    pub fn batch_sizes(&self) -> Vec<usize> {
        self.lock().batches.clone()
    }

    /// Como un anfitrión que no recrea mandos tal cual.
    pub fn without_raw(self) -> Self {
        self.lock().raw_unsupported = true;
        self
    }

    /// Simula un informe de salida del driver del anfitrión para el mando tal cual de `slot`.
    pub fn push_output(&self, slot: u8, data: Vec<u8>) {
        self.lock().outputs.push((slot, data));
    }

    /// Espera hasta que `condition` se cumpla sobre el registro o pase `timeout`; devuelve si se
    /// cumplió.
    pub fn wait_for(&self, timeout: Duration, condition: impl Fn(&[Recorded]) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if condition(&self.lock().log) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl InputSink for RecordingSink {
    fn inject(&mut self, event: &InputEvent, guest: GuestId) -> Result<(), InputError> {
        let mut state = self.lock();
        if let crate::EventClass::Gamepad(slot) = event.class()
            && !state.slots.contains(&slot)
        {
            return Err(InputError::NoSuchSlot(slot));
        }
        state.log.push(Recorded::Event {
            guest,
            event: *event,
        });
        Ok(())
    }

    fn inject_batch(
        &mut self,
        events: &[InputEvent],
        guest: GuestId,
        errors: &mut Vec<InputError>,
    ) -> usize {
        self.lock().batches.push(events.len());
        let mut applied = 0;
        for event in events {
            match self.inject(event, guest) {
                Ok(()) => applied += 1,
                Err(err) => errors.push(err),
            }
        }
        applied
    }

    fn release_all(&mut self, guest: GuestId) -> Result<(), InputError> {
        self.lock().log.push(Recorded::ReleaseAll { guest });
        Ok(())
    }

    fn assign_gamepad(&mut self, slot: u8) -> Result<(), InputError> {
        let mut state = self.lock();
        state.slots.insert(slot);
        state.log.push(Recorded::GamepadAssigned { slot });
        Ok(())
    }

    fn release_gamepad(&mut self, slot: u8) -> Result<(), InputError> {
        let mut state = self.lock();
        if !state.slots.remove(&slot) {
            return Err(InputError::NoSuchSlot(slot));
        }
        state.raw.remove(&slot);
        state.log.push(Recorded::GamepadReleased { slot });
        Ok(())
    }

    fn describe(&self) -> String {
        "registro de prueba".to_owned()
    }

    fn connect_raw(&mut self, slot: u8, device: &GamepadDevice) -> Result<(), InputError> {
        let mut state = self.lock();
        if state.raw_unsupported {
            return Err(InputError::Unsupported(
                "sumidero de prueba sin tal cual".into(),
            ));
        }
        if !state.slots.contains(&slot) {
            return Err(InputError::NoSuchSlot(slot));
        }
        state.raw.insert(slot);
        state.log.push(Recorded::RawConnected {
            slot,
            vendor: device.vendor,
            product: device.product,
            hid: matches!(device.transport, GamepadTransport::Hid { .. }),
        });
        Ok(())
    }

    fn raw_report(&mut self, slot: u8, data: &[u8]) -> Result<(), InputError> {
        let mut state = self.lock();
        if !state.raw.contains(&slot) {
            return Err(InputError::NoSuchSlot(slot));
        }
        state.log.push(Recorded::RawReport {
            slot,
            len: data.len(),
        });
        Ok(())
    }

    fn disconnect_raw(&mut self, slot: u8) -> Result<(), InputError> {
        let mut state = self.lock();
        if state.raw.remove(&slot) {
            state.log.push(Recorded::RawDisconnected { slot });
        }
        Ok(())
    }

    fn take_outputs(&mut self) -> Vec<(u8, Vec<u8>)> {
        std::mem::take(&mut self.lock().outputs)
    }
}

/// Umbral de los sticks y gatillos del perfil de [`GamepadToKeys`] (la mitad del recorrido).
pub const KEYS_THRESHOLD: i16 = i16::MAX / 2;

/// Mapeo de mando a teclas (`--gamepad-to-keys`): en Windows, la alternativa a instalar el driver
/// de mando virtual (`drivers/windows-vhid`).
///
/// Perfil fijo, igual para todas las ranuras (todos los mandos escriben en el mismo teclado):
///
/// | Mando | Tecla |
/// |---|---|
/// | Stick izquierdo (más de la mitad) | W, A, S, D |
/// | Cruceta | flechas |
/// | A | Espacio |
/// | B | E |
/// | X | R |
/// | Y | F |
/// | LB | Q |
/// | RB | Tab |
/// | LT (más de la mitad) | Mayús izquierda |
/// | RT (más de la mitad) | Ctrl izquierda |
/// | Back | Esc |
/// | Start | Intro |
///
/// El stick derecho, LS, RS y Guide no se mapean.
pub struct GamepadToKeys<S> {
    inner: S,
    /// Teclas que mantiene pulsadas cada ranura por sus ejes.
    axis_keys: BTreeMap<(u8, u16), bool>,
    /// Lo traducido de un grupo (reutilizado).
    scratch: Vec<InputEvent>,
}

impl<S: InputSink> GamepadToKeys<S> {
    /// Envuelve el sumidero de teclado y ratón.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            axis_keys: BTreeMap::new(),
            scratch: Vec::with_capacity(64),
        }
    }

    /// Tecla del perfil para un botón.
    pub fn key_for_button(button: GamepadButton) -> Option<u16> {
        Some(match button {
            GamepadButton::A => codes::KEY_SPACE,
            GamepadButton::B => codes::KEY_E,
            GamepadButton::X => codes::KEY_R,
            GamepadButton::Y => codes::KEY_F,
            GamepadButton::LeftBumper => codes::KEY_Q,
            GamepadButton::RightBumper => codes::KEY_TAB,
            GamepadButton::Back => codes::KEY_ESC,
            GamepadButton::Start => codes::KEY_ENTER,
            GamepadButton::DPadUp => codes::KEY_UP,
            GamepadButton::DPadDown => codes::KEY_DOWN,
            GamepadButton::DPadLeft => codes::KEY_LEFT,
            GamepadButton::DPadRight => codes::KEY_RIGHT,
            GamepadButton::Guide | GamepadButton::LeftStick | GamepadButton::RightStick => {
                return None;
            }
        })
    }

    /// Traduce `event` a lo que recibe el sumidero interior y lo añade a `out` (un mando puede no
    /// producir nada: botones sin tecla, ejes que no cruzan el umbral, conexiones).
    fn translate(&mut self, event: &InputEvent, out: &mut Vec<InputEvent>) {
        match *event {
            InputEvent::GamepadButton {
                button, pressed, ..
            } => {
                if let Some(code) = Self::key_for_button(button) {
                    out.push(InputEvent::Key { code, pressed });
                }
            }
            InputEvent::GamepadAxis { slot, axis, value } => {
                let pairs: &[(u16, bool)] = match axis {
                    GamepadAxis::LeftX => &[
                        (codes::KEY_D, value > KEYS_THRESHOLD),
                        (codes::KEY_A, value < -KEYS_THRESHOLD),
                    ],
                    GamepadAxis::LeftY => &[
                        (codes::KEY_W, value > KEYS_THRESHOLD),
                        (codes::KEY_S, value < -KEYS_THRESHOLD),
                    ],
                    GamepadAxis::LeftTrigger => &[(codes::KEY_LEFTSHIFT, value > KEYS_THRESHOLD)],
                    GamepadAxis::RightTrigger => &[(codes::KEY_LEFTCTRL, value > KEYS_THRESHOLD)],
                    GamepadAxis::RightX | GamepadAxis::RightY => &[],
                };
                for (code, down) in pairs {
                    let previous = self.axis_keys.insert((slot, *code), *down).unwrap_or(false);
                    if previous != *down {
                        out.push(InputEvent::Key {
                            code: *code,
                            pressed: *down,
                        });
                    }
                }
            }
            InputEvent::GamepadConnected { .. } | InputEvent::GamepadDisconnected { .. } => {}
            _ => out.push(*event),
        }
    }
}

impl<S: InputSink> InputSink for GamepadToKeys<S> {
    fn inject(&mut self, event: &InputEvent, guest: GuestId) -> Result<(), InputError> {
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        self.translate(event, &mut out);
        let result = out
            .iter()
            .try_for_each(|event| self.inner.inject(event, guest));
        self.scratch = out;
        result
    }

    /// Traduce el grupo entero y se lo pasa junto al sumidero interior (las teclas de un mando
    /// salen en una sola llamada).
    fn inject_batch(
        &mut self,
        events: &[InputEvent],
        guest: GuestId,
        errors: &mut Vec<InputError>,
    ) -> usize {
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        for event in events {
            self.translate(event, &mut out);
        }
        let applied = self.inner.inject_batch(&out, guest, errors);
        let failed = out.len() - applied;
        self.scratch = out;
        events.len().saturating_sub(failed)
    }

    fn release_all(&mut self, guest: GuestId) -> Result<(), InputError> {
        // Las teclas de los ejes se anotan como pulsadas por `guest` en el sumidero interior:
        // `release_all` las suelta con las demás.
        self.axis_keys.clear();
        self.inner.release_all(guest)
    }

    fn assign_gamepad(&mut self, _slot: u8) -> Result<(), InputError> {
        Ok(())
    }

    fn release_gamepad(&mut self, slot: u8) -> Result<(), InputError> {
        self.axis_keys.retain(|(s, _), _| *s != slot);
        Ok(())
    }

    fn prepare(&mut self) {
        self.inner.prepare();
    }

    fn replace_eis(&mut self, eis: EisProvider) {
        self.inner.replace_eis(eis);
    }

    fn tick(&mut self, now: Instant) -> Option<Instant> {
        self.inner.tick(now)
    }

    fn describe(&self) -> String {
        format!("{} con mandos como teclas", self.inner.describe())
    }

    fn connect_raw(&mut self, _slot: u8, _device: &GamepadDevice) -> Result<(), InputError> {
        Err(InputError::Unsupported(
            "el anfitrión convierte los mandos en teclas (--gamepad-to-keys)".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: GuestId = GuestId(1);
    const TWO: GuestId = GuestId(2);

    #[test]
    fn input_sink_is_send_and_object_safe() {
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<dyn InputSink>();
        let mut sink: Box<dyn InputSink> = Box::new(RecordingSink::new());
        let event = InputEvent::Key {
            code: 30,
            pressed: true,
        };
        assert_eq!(sink.inject(&event, ONE), Ok(()));
        assert_eq!(sink.release_all(ONE), Ok(()));
    }

    #[test]
    fn shared_keys_are_released_by_the_last_holder() {
        let mut pressed = PressedKeys::default();
        assert!(pressed.key(ONE, 30, true), "primera pulsación");
        assert!(!pressed.key(TWO, 30, true), "ya estaba pulsada");
        assert!(!pressed.key(ONE, 30, false), "la sigue pulsando el otro");
        assert!(pressed.key(TWO, 30, false), "la suelta el último");
        assert!(
            !pressed.key(TWO, 30, false),
            "soltar dos veces no hace nada"
        );

        assert!(pressed.key(ONE, 17, true));
        assert!(pressed.key(ONE, 31, true));
        assert!(!pressed.key(TWO, 31, true));
        assert!(pressed.button(ONE, MouseButton::Left, true));
        let (keys, buttons) = pressed.release_guest(ONE);
        assert_eq!(keys, vec![17], "31 lo sigue pulsando el invitado 2");
        assert_eq!(buttons, vec![MouseButton::Left]);
        let (keys, _) = pressed.release_guest(TWO);
        assert_eq!(keys, vec![31]);
    }

    #[test]
    fn the_last_pressed_key_repeats_at_the_system_cadence() {
        // Retardo 1 (500 ms) y velocidad 31 (~30 Hz): un intervalo de unos 33 ms.
        let mut repeat = KeyRepeat::from_windows_settings(1, 31);
        assert_eq!(repeat.delay, Duration::from_millis(500));
        assert!(
            (32..=34).contains(&repeat.interval.as_millis()),
            "{repeat:?}"
        );
        let slow = KeyRepeat::from_windows_settings(0, 0);
        assert_eq!(slow.delay, Duration::from_millis(250));
        assert_eq!(slow.interval, Duration::from_millis(400));
        let clamped = KeyRepeat::from_windows_settings(9, 99);
        assert_eq!(clamped.delay, Duration::from_millis(1000));
        assert_eq!(clamped.interval, slow.interval.min(clamped.interval));

        let t0 = Instant::now();
        assert_eq!(repeat.due(t0), None, "sin tecla no hay repetición");
        repeat.pressed(codes::KEY_A, t0);
        assert_eq!(repeat.next(), Some(t0 + Duration::from_millis(500)));
        assert_eq!(repeat.due(t0 + Duration::from_millis(499)), None);
        assert_eq!(
            repeat.due(t0 + Duration::from_millis(500)),
            Some(codes::KEY_A)
        );
        assert_eq!(
            repeat.due(t0 + Duration::from_millis(501)),
            None,
            "una por cita"
        );
        assert_eq!(
            repeat.due(t0 + Duration::from_millis(500) + repeat.interval),
            Some(codes::KEY_A)
        );
        // Muy tarde: no se acumulan repeticiones; la siguiente va desde ahora.
        let late = t0 + Duration::from_secs(3);
        assert_eq!(repeat.due(late), Some(codes::KEY_A));
        assert_eq!(repeat.due(late + Duration::from_millis(1)), None);
        assert!(repeat.next().unwrap() > late);

        // Otra tecla pulsada: se repite esa, con el retardo entero; soltar la anterior no cambia
        // nada; soltar la actual para la repetición aunque la otra siga pulsada.
        repeat.pressed(codes::KEY_S, late);
        assert_eq!(repeat.due(late + Duration::from_millis(100)), None);
        repeat.released(codes::KEY_A);
        assert_eq!(
            repeat.due(late + Duration::from_millis(500)),
            Some(codes::KEY_S)
        );
        repeat.released(codes::KEY_S);
        assert_eq!(repeat.next(), None);

        // Los modificadores y los bloqueos no se repiten, y pulsarlos corta la repetición de la
        // tecla mantenida (A y luego Mayús no sigue escribiendo Mayús+A).
        repeat.pressed(codes::KEY_A, late);
        assert_eq!(repeat.next(), Some(late + Duration::from_millis(500)));
        repeat.pressed(codes::KEY_LEFTSHIFT, late);
        assert_eq!(repeat.next(), None, "Mayús para la repetición de A");
        assert_eq!(repeat.due(late + Duration::from_secs(1)), None);
        repeat.pressed(codes::KEY_CAPSLOCK, late);
        assert_eq!(repeat.next(), None);
        assert!(KeyRepeat::repeats(codes::KEY_SPACE) && !KeyRepeat::repeats(codes::KEY_LEFTMETA));
        repeat.pressed(codes::KEY_SPACE, late);
        repeat.clear();
        assert_eq!(repeat.next(), None);
    }

    /// El agrupado por defecto aplica uno a uno y cuenta lo que falló.
    #[test]
    fn the_default_batch_counts_failures() {
        let mut sink = RecordingSink::new();
        let mut errors = Vec::new();
        let events = [
            InputEvent::Key {
                code: 30,
                pressed: true,
            },
            InputEvent::GamepadButton {
                slot: 2,
                button: GamepadButton::A,
                pressed: true,
            },
            InputEvent::MouseMove { dx: 1, dy: 1 },
        ];
        assert_eq!(sink.inject_batch(&events, ONE, &mut errors), 2);
        assert_eq!(errors, vec![InputError::NoSuchSlot(2)]);
        assert_eq!(sink.events_of(ONE).len(), 2);
        assert_eq!(sink.batch_sizes(), vec![3]);
        assert_eq!(sink.tick(Instant::now()), None);
    }

    #[test]
    fn pads_return_to_rest() {
        let mut pads = PadTracker::default();
        pads.note(
            ONE,
            &InputEvent::GamepadButton {
                slot: 2,
                button: GamepadButton::A,
                pressed: true,
            },
        );
        pads.note(
            ONE,
            &InputEvent::GamepadAxis {
                slot: 2,
                axis: GamepadAxis::LeftX,
                value: 1000,
            },
        );
        pads.note(
            TWO,
            &InputEvent::GamepadButton {
                slot: 3,
                button: GamepadButton::B,
                pressed: true,
            },
        );
        let neutral = pads.neutral_events(Some(ONE), None);
        assert_eq!(
            neutral,
            vec![
                InputEvent::GamepadButton {
                    slot: 2,
                    button: GamepadButton::A,
                    pressed: false
                },
                InputEvent::GamepadAxis {
                    slot: 2,
                    axis: GamepadAxis::LeftX,
                    value: 0
                }
            ]
        );
        assert!(pads.neutral_events(Some(ONE), None).is_empty());
        assert_eq!(pads.neutral_events(None, Some(3)).len(), 1);
    }

    #[test]
    fn the_recording_sink_needs_an_assigned_slot() {
        let mut sink = RecordingSink::new();
        let pad = InputEvent::GamepadButton {
            slot: 2,
            button: GamepadButton::Y,
            pressed: true,
        };
        assert_eq!(sink.inject(&pad, ONE), Err(InputError::NoSuchSlot(2)));
        sink.assign_gamepad(2).unwrap();
        assert_eq!(sink.inject(&pad, ONE), Ok(()));
        assert_eq!(sink.assigned_slots(), vec![2]);
        sink.release_gamepad(2).unwrap();
        assert_eq!(sink.release_gamepad(2), Err(InputError::NoSuchSlot(2)));
        assert_eq!(sink.events_of(ONE), vec![pad]);
        assert_eq!(
            sink.log(),
            vec![
                Recorded::GamepadAssigned { slot: 2 },
                Recorded::Event {
                    guest: ONE,
                    event: pad
                },
                Recorded::GamepadReleased { slot: 2 },
            ]
        );
        assert!(sink.wait_for(Duration::from_millis(1), |log| log.len() == 3));
    }

    #[test]
    fn gamepads_map_to_the_documented_keys() {
        let recording = RecordingSink::new();
        let mut sink = GamepadToKeys::new(recording.clone());
        sink.assign_gamepad(1).unwrap();
        let events = [
            InputEvent::GamepadButton {
                slot: 1,
                button: GamepadButton::A,
                pressed: true,
            },
            InputEvent::GamepadAxis {
                slot: 1,
                axis: GamepadAxis::LeftY,
                value: 30_000,
            },
            InputEvent::GamepadAxis {
                slot: 1,
                axis: GamepadAxis::LeftY,
                value: 31_000,
            },
            InputEvent::GamepadAxis {
                slot: 1,
                axis: GamepadAxis::LeftY,
                value: 0,
            },
            InputEvent::GamepadButton {
                slot: 1,
                button: GamepadButton::Guide,
                pressed: true,
            },
            InputEvent::MouseMove { dx: 3, dy: 4 },
        ];
        for event in &events {
            sink.inject(event, ONE).unwrap();
        }
        let expected = vec![
            InputEvent::Key {
                code: codes::KEY_SPACE,
                pressed: true,
            },
            InputEvent::Key {
                code: codes::KEY_W,
                pressed: true,
            },
            InputEvent::Key {
                code: codes::KEY_W,
                pressed: false,
            },
            InputEvent::MouseMove { dx: 3, dy: 4 },
        ];
        assert_eq!(recording.events_of(ONE), expected);

        // En grupo, lo mismo: los eventos sin tecla cuentan como aplicados.
        let recording = RecordingSink::new();
        let mut sink = GamepadToKeys::new(recording.clone());
        sink.assign_gamepad(1).unwrap();
        let mut errors = Vec::new();
        assert_eq!(sink.inject_batch(&events, ONE, &mut errors), events.len());
        assert!(errors.is_empty());
        assert_eq!(recording.events_of(ONE), expected);
    }
}
