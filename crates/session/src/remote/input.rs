//! Entrada del invitado en la sesión remota.
//!
//! **Anfitrión** ([`HostInput`]): por cada invitado con secreto pareja a pareja guarda un
//! [`InputCipher`] indexado por su `key_id`. Por cada conexión (la directa de cada invitado y la del
//! relay, que trae a varios) hay dos tareas: una lee los datagramas de la pista `Input` (separados
//! de los de medios por la propia conexión) y otra acepta los streams de entrada que abre el
//! invitado. Las dos abren el sellado (que autentica al invitado: sin su clave no abre), miden la
//! latencia `recibido − enviado` con el reloj del `MediaSender` de ese invitado (el que contesta a
//! sus `Ping`) y pasan los eventos al hilo `inject`, el único que toca el [`InputSink`]. Ese hilo
//! lleva el estado autoritativo de permisos y pausa, así que todo pasa en orden: lo no permitido se
//! descarta y se cuenta, la pausa descarta todo y suelta lo pulsado (`release_all`), al quitar o
//! cambiar la ranura se destruye o crea el mando, y al expulsar o perder a un invitado se suelta
//! lo suyo y se libera su ranura. Los datagramas más antiguos que el último aplicado se descartan.
//! Los contadores y la latencia de cada invitado salen en su `GuestStatus` y en el resumen.
//!
//! **Invitado** ([`GuestInput`]): con la conexión abierta y su secreto pareja a pareja, abre el
//! stream de entrada (preámbulo con su `key_id`), crea el [`InputSender`] y lo deja listo para la
//! ventana o el guion. Recibe `InputPermissions`, `HostFrameSize` e `InputReport` por el control.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use coroom_input::capture::Allowed;
use coroom_input::gamepad::PadLink;
use coroom_input::inject::{EisConnection, EisProvider, PointerArea};
use coroom_input::script::ScriptTarget;
use coroom_input::send::{InputLink, InputSender, SenderSnapshot};
use coroom_input::{EventClass, InputError, InputEvent, InputSink};
use coroom_transport::PeerConnection;
use coroom_transport::crypto::hpke::PairwiseSecret;
use coroom_transport::crypto::input::{Channel, InputCipher, Role};
use coroom_transport::media::{MediaReceiver, MediaSender};
use coroom_transport::proto::hid;
use coroom_transport::proto::input::{
    GamepadDevice, GamepadTransport, InputMessage, InputPacket, PadData, STREAM_PREAMBLE_LEN,
    StreamFrames, parse_datagram, parse_stream_preamble, seq_after, stream_preamble,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use tracing::debug;

use super::{Command, Core, GuestPad, PadTransport, lock};
use crate::latency::InputLatency;
use crate::{GuestId, Permissions, SessionError};

/// Espera del anfitrión a que llegue la clave de un stream de entrada (la oferta HPKE viaja por la
/// señalización y puede llegar después de la conexión).
pub const KEY_WAIT: Duration = Duration::from_secs(5);

/// Muestras de latencia que guarda el anfitrión por invitado.
const LATENCY_WINDOW: usize = 2048;

/// Un desfase de reloj negativo mayor que esto descarta la muestra (reloj aún sin converger).
const MAX_NEGATIVE_US: i64 = 2_000;

/// Socket de EI de la captura (`coroom_capture::RemoteDesktopShare`) como proveedor de la inyección:
/// la captura abre la sesión `RemoteDesktop` después de la suya y dice qué se transmite.
pub fn eis_provider(share: coroom_capture::RemoteDesktopShare) -> EisProvider {
    Box::new(move || {
        share
            .wait(Duration::from_secs(300))
            .map(|socket| EisConnection {
                #[cfg(unix)]
                fd: socket.fd,
                area: pointer_area(socket.area),
            })
    })
}

fn pointer_area(area: coroom_capture::SharedArea) -> PointerArea {
    match area {
        coroom_capture::SharedArea::Unknown => PointerArea::Unknown,
        coroom_capture::SharedArea::Monitor { x, y } => PointerArea::Monitor { x, y },
        coroom_capture::SharedArea::Window { width, height } => {
            PointerArea::Window { width, height }
        }
    }
}

// --- Anfitrión ---------------------------------------------------------------------------------

/// Trabajo del hilo `inject`.
pub(crate) enum Job {
    /// Eventos de un invitado ya autenticados.
    Packet {
        guest: GuestId,
        datagram: bool,
        seq: u32,
        events: Vec<InputEvent>,
        pads: Vec<PadData>,
        latency_us: Option<u64>,
    },
    /// Permisos nuevos.
    Permissions {
        guest: GuestId,
        permissions: Permissions,
    },
    /// Pausa global.
    Paused(bool),
    /// El invitado salió, fue expulsado o perdió la conexión.
    GuestGone(GuestId),
}

struct KeyEntry {
    guest: GuestId,
    cipher: Arc<Mutex<InputCipher>>,
}

/// Entrada del anfitrión: claves por invitado, lectores y el hilo `inject`.
pub(crate) struct HostInput {
    jobs: Sender<Job>,
    keys: Mutex<HashMap<u32, KeyEntry>>,
    /// Datagramas o tramas que no abren (otra clave, manipulados o repetidos).
    rejected: AtomicU64,
}

impl HostInput {
    /// Arranca el hilo `inject` con `sink`.
    pub(crate) fn start(
        core: &Arc<Core>,
        sink: Box<dyn InputSink>,
    ) -> Result<(Arc<HostInput>, JoinHandle<()>), SessionError> {
        let (jobs, rx) = unbounded();
        let worker_core = Arc::clone(core);
        let thread = std::thread::Builder::new()
            .name("inject".into())
            .spawn(move || Worker::new(worker_core, sink).run(&rx))
            .map_err(|err| {
                SessionError::Media(format!("no se pudo crear el hilo inject: {err}"))
            })?;
        Ok((
            Arc::new(HostInput {
                jobs,
                keys: Mutex::new(HashMap::new()),
                rejected: AtomicU64::new(0),
            }),
            thread,
        ))
    }

    pub(crate) fn job(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    /// Instala la clave de entrada de `guest`.
    pub(crate) fn add_guest(&self, guest: GuestId, secret: &PairwiseSecret) -> Result<(), String> {
        let cipher =
            InputCipher::from_pairwise(secret, Role::Host).map_err(|err| err.to_string())?;
        let mut keys = lock(&self.keys);
        keys.retain(|_, entry| entry.guest != guest);
        keys.insert(
            cipher.key_id(),
            KeyEntry {
                guest,
                cipher: Arc::new(Mutex::new(cipher)),
            },
        );
        Ok(())
    }

    /// Olvida la clave de `guest` y suelta lo suyo.
    pub(crate) fn remove_guest(&self, guest: GuestId) {
        lock(&self.keys).retain(|_, entry| entry.guest != guest);
        self.job(Job::GuestGone(guest));
    }

    fn key(&self, key_id: u32) -> Option<(GuestId, Arc<Mutex<InputCipher>>)> {
        lock(&self.keys)
            .get(&key_id)
            .map(|entry| (entry.guest, Arc::clone(&entry.cipher)))
    }

    /// Lanza los lectores de entrada de `conn` (solo de `only` si es la conexión directa de un
    /// invitado; de cualquiera si es la del relay). `clock` es el emisor de medios de esa
    /// conexión, cuyo reloj es el que sincroniza el invitado.
    pub(crate) fn spawn_readers(
        self: &Arc<Self>,
        core: &Arc<Core>,
        conn: &PeerConnection,
        only: Option<GuestId>,
        clock: &Arc<MediaSender>,
    ) {
        tokio::spawn(datagram_reader(
            Arc::clone(self),
            conn.clone(),
            only,
            Arc::clone(clock),
        ));
        tokio::spawn(stream_acceptor(
            Arc::clone(self),
            Arc::clone(core),
            conn.clone(),
            only,
            Arc::clone(clock),
        ));
    }
}

/// `recibido − enviado` en µs del reloj de `clock` (truncado a 32 bits como `sent_us`).
fn latency_us(clock: &MediaSender, received: Instant, sent_us: u32) -> Option<u64> {
    if sent_us == 0 {
        return None;
    }
    let now = clock.clock_us(received) as u32;
    let delta = i64::from(now.wrapping_sub(sent_us) as i32);
    (delta >= -MAX_NEGATIVE_US).then(|| delta.max(0) as u64)
}

async fn datagram_reader(
    input: Arc<HostInput>,
    conn: PeerConnection,
    only: Option<GuestId>,
    clock: Arc<MediaSender>,
) {
    let mut out = Vec::with_capacity(256);
    while let Ok(datagram) = conn.read_input_datagram().await {
        let received = Instant::now();
        let Ok((key_id, sealed)) = parse_datagram(&datagram) else {
            continue;
        };
        let Some((guest, cipher)) = input.key(key_id) else {
            input.rejected.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if only.is_some_and(|only| only != guest) {
            input.rejected.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let packet = {
            let mut cipher = lock(&cipher);
            match cipher.open(sealed, &mut out) {
                Ok(opened) if opened.channel == Channel::Datagram => {
                    InputPacket::from_bytes(opened.plaintext)
                        .ok()
                        .filter(|packet| u64::from(packet.seq) == opened.seq)
                }
                _ => None,
            }
        };
        let Some(packet) = packet else {
            input.rejected.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        input.job(Job::Packet {
            guest,
            datagram: true,
            seq: packet.seq,
            latency_us: latency_us(&clock, received, packet.sent_us),
            events: packet.events,
            pads: packet.pads,
        });
    }
}

async fn stream_acceptor(
    input: Arc<HostInput>,
    core: Arc<Core>,
    conn: PeerConnection,
    only: Option<GuestId>,
    clock: Arc<MediaSender>,
) {
    while let Ok((send, recv)) = conn.accept_input().await {
        tokio::spawn(stream_reader(
            Arc::clone(&input),
            Arc::clone(&core),
            send,
            recv,
            only,
            Arc::clone(&clock),
        ));
    }
}

async fn stream_reader(
    input: Arc<HostInput>,
    core: Arc<Core>,
    _send: coroom_transport::SendStream,
    mut recv: coroom_transport::RecvStream,
    only: Option<GuestId>,
    clock: Arc<MediaSender>,
) {
    let mut preamble = [0u8; STREAM_PREAMBLE_LEN];
    match tokio::time::timeout(KEY_WAIT, recv.read_exact(&mut preamble)).await {
        Ok(Ok(())) => {}
        _ => return,
    }
    let Ok(key_id) = parse_stream_preamble(preamble) else {
        return;
    };
    let deadline = Instant::now() + KEY_WAIT;
    let (guest, cipher) = loop {
        if let Some(found) = input.key(key_id) {
            break found;
        }
        if Instant::now() >= deadline {
            let who = only.map_or_else(
                || "un invitado".to_owned(),
                |g| format!("el invitado {}", g.0),
            );
            core.warn(format!(
                "se rechaza la entrada de {who}: no estableció la clave pareja a pareja"
            ));
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    if only.is_some_and(|only| only != guest) {
        return;
    }
    let mut frames = StreamFrames::new();
    let mut out = Vec::with_capacity(256);
    loop {
        let chunk = match recv.read_chunk(64 * 1024, true).await {
            Ok(Some(chunk)) => chunk,
            Ok(None) | Err(_) => return,
        };
        let received = Instant::now();
        frames.extend(&chunk.bytes);
        loop {
            let sealed = match frames.next_frame() {
                Ok(Some(sealed)) => sealed,
                Ok(None) => break,
                Err(err) => {
                    debug!(error = %err, guest = guest.0, "stream de entrada no válido");
                    return;
                }
            };
            let message = {
                let mut cipher = lock(&cipher);
                match cipher.open(&sealed, &mut out) {
                    Ok(opened) if opened.channel == Channel::Stream => {
                        InputMessage::from_bytes(opened.plaintext)
                            .ok()
                            .filter(|message| u64::from(message.seq) == opened.seq)
                    }
                    _ => None,
                }
            };
            let Some(message) = message else {
                input.rejected.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            input.job(Job::Packet {
                guest,
                datagram: false,
                seq: message.seq,
                latency_us: latency_us(&clock, received, message.sent_us),
                events: message.events,
                pads: message.pads,
            });
        }
    }
}

/// El mando físico que declaró un invitado para su ranura.
struct PadState {
    slot: u8,
    device: GamepadDevice,
    /// Si el sumidero lo tiene tal cual ahora.
    active: bool,
    /// Por qué no va tal cual, si se pidió.
    note: Option<String>,
}

impl PadState {
    fn status(&self) -> GuestPad {
        GuestPad {
            name: self.device.name.clone(),
            vendor: self.device.vendor,
            product: self.device.product,
            transport: match self.device.transport {
                GamepadTransport::Hid { .. } => PadTransport::Hid,
                GamepadTransport::XInput => PadTransport::XInput,
                GamepadTransport::Converted { .. } => PadTransport::Converted,
            },
            raw: self.active,
            note: self.note.clone(),
        }
    }

    /// Si el invitado tiene que mandar los informes en crudo (si no, sus eventos convertidos).
    fn guest_sends_raw(&self) -> bool {
        self.active && matches!(self.device.transport, GamepadTransport::Hid { .. })
    }
}

/// Si el mando declarado se puede crear tal cual con estos permisos.
fn check_device(device: &GamepadDevice, permissions: &Permissions) -> Result<(), String> {
    match &device.transport {
        GamepadTransport::Hid { descriptor, .. } => {
            let summary = hid::parse(descriptor).map_err(|err| err.to_string())?;
            if !summary.has_controller() {
                return Err("su descriptor no declara un mando".to_owned());
            }
            if summary.has_keyboard_or_mouse() && !permissions.keyboard_mouse {
                return Err(
                    "también se declara teclado o ratón y el invitado no tiene permiso de \
                     teclado y ratón"
                        .to_owned(),
                );
            }
            Ok(())
        }
        GamepadTransport::XInput => Ok(()),
        GamepadTransport::Converted { reason } => Err(reason.clone()),
    }
}

/// Estado de entrada de un invitado en el hilo `inject`.
#[derive(Default)]
struct GuestState {
    permissions: Permissions,
    pad: Option<PadState>,
    last_datagram: Option<u32>,
    applied: u64,
    dropped: u64,
    latencies: VecDeque<u64>,
    dirty: bool,
}

/// El hilo `inject`: único dueño del sumidero.
struct Worker {
    core: Arc<Core>,
    sink: Box<dyn InputSink>,
    guests: BTreeMap<GuestId, GuestState>,
    paused: bool,
    warned: Vec<String>,
    totals: (u64, u64),
}

impl Worker {
    fn new(core: Arc<Core>, sink: Box<dyn InputSink>) -> Self {
        Self {
            core,
            sink,
            guests: BTreeMap::new(),
            paused: false,
            warned: Vec::new(),
            totals: (0, 0),
        }
    }

    /// Avisa una vez de cada error distinto del sumidero.
    fn warn(&mut self, err: &InputError) {
        let text = err.to_string();
        if !self.warned.contains(&text) {
            self.core.warn(format!("entrada: {text}"));
            self.warned.push(text);
        }
    }

    fn run(mut self, rx: &Receiver<Job>) {
        debug!(backend = self.sink.describe(), "hilo inject");
        loop {
            // Con un mando tal cual, su vibración sale enseguida aunque no llegue nada.
            let wait = if self.any_raw() {
                Duration::from_millis(5)
            } else {
                Duration::from_millis(100)
            };
            let received = rx.recv_timeout(wait);
            self.forward_outputs();
            match received {
                Ok(job) => {
                    self.handle(job);
                    for _ in 0..256 {
                        match rx.try_recv() {
                            Ok(job) => self.handle(job),
                            Err(_) => break,
                        }
                    }
                    self.publish();
                    // Con entrada continua nunca vence la espera: se mira aquí también.
                    if self.core.shared.stopping() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.core.shared.stopping() || self.core.ended() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        // Al terminar no puede quedar nada pulsado ni ningún mando creado.
        let guests: Vec<GuestId> = self.guests.keys().copied().collect();
        for guest in guests {
            self.gone(guest);
        }
        self.publish();
    }

    fn handle(&mut self, job: Job) {
        match job {
            Job::Packet {
                guest,
                datagram,
                seq,
                events,
                pads,
                latency_us,
            } => {
                self.packet(guest, datagram, seq, &events, latency_us);
                self.pads(guest, pads);
            }
            Job::Permissions { guest, permissions } => self.permissions(guest, permissions),
            Job::Paused(paused) => {
                let was = self.paused;
                self.paused = paused;
                let guests: Vec<GuestId> = self.guests.keys().copied().collect();
                if paused && !was {
                    for guest in &guests {
                        if let Err(err) = self.sink.release_all(*guest) {
                            self.warn(&err);
                        }
                    }
                }
                // Un mando tal cual no tiene un informe «en reposo» que inyectar: en pausa se
                // quita (el sistema suelta lo que tuviera pulsado) y vuelve al reanudar.
                if paused != was {
                    for guest in guests {
                        self.apply_pad_mode(guest);
                    }
                }
            }
            Job::GuestGone(guest) => self.gone(guest),
        }
    }

    fn packet(
        &mut self,
        guest: GuestId,
        datagram: bool,
        seq: u32,
        events: &[InputEvent],
        latency_us: Option<u64>,
    ) {
        let paused = self.paused;
        let state = self.guests.entry(guest).or_default();
        state.dirty = true;
        if let Some(us) = latency_us {
            state.latencies.push_back(us);
            while state.latencies.len() > LATENCY_WINDOW {
                state.latencies.pop_front();
            }
            self.core.ledger.record_input_latency(us);
        }
        let count = events.len() as u64;
        if paused {
            state.dropped += count;
            self.totals.1 += count;
            return;
        }
        if datagram {
            // El movimiento más antiguo que el último aplicado ya no sirve.
            if state
                .last_datagram
                .is_some_and(|last| !seq_after(seq, last))
            {
                state.dropped += count;
                self.totals.1 += count;
                return;
            }
            state.last_datagram = Some(seq);
        }
        let permissions = state.permissions;
        let mut errors = Vec::new();
        let (mut applied, mut dropped) = (0u64, 0u64);
        for event in events {
            let allowed = match event.class() {
                EventClass::KeyboardMouse => permissions.keyboard_mouse,
                EventClass::Gamepad(slot) => permissions.gamepad_slot == Some(slot),
                EventClass::Control => true,
            };
            if !allowed {
                dropped += 1;
                continue;
            }
            if let InputEvent::GamepadDisconnected { slot } = event {
                self.forget_pad(guest, *slot);
            }
            let result = match event {
                InputEvent::ReleaseAll => self.sink.release_all(guest),
                _ => self.sink.inject(event, guest),
            };
            match result {
                Ok(()) => applied += 1,
                Err(err) => {
                    dropped += 1;
                    errors.push(err);
                }
            }
        }
        if let Some(state) = self.guests.get_mut(&guest) {
            state.applied += applied;
            state.dropped += dropped;
        }
        self.totals.0 += applied;
        self.totals.1 += dropped;
        for err in errors {
            self.warn(&err);
        }
    }

    fn permissions(&mut self, guest: GuestId, permissions: Permissions) {
        let old = self
            .guests
            .get(&guest)
            .map(|state| state.permissions)
            .unwrap_or_default();
        let state = self.guests.entry(guest).or_default();
        state.permissions = permissions;
        state.dirty = true;
        let lost_kbm = old.keyboard_mouse && !permissions.keyboard_mouse;
        let slot_changed = old.gamepad_slot != permissions.gamepad_slot;
        let mut errors = Vec::new();
        if (lost_kbm || (slot_changed && old.gamepad_slot.is_some()))
            && let Err(err) = self.sink.release_all(guest)
        {
            errors.push(err);
        }
        if slot_changed {
            if let Some(slot) = old.gamepad_slot
                && let Err(err) = self.sink.release_gamepad(slot)
            {
                errors.push(err);
            }
            if let Some(slot) = permissions.gamepad_slot
                && let Err(err) = self.sink.assign_gamepad(slot)
            {
                errors.push(err);
            }
        }
        if permissions.keyboard_mouse && !old.keyboard_mouse {
            self.sink.prepare();
        }
        for err in errors {
            self.warn(&err);
        }
        if slot_changed {
            // El invitado vuelve a declarar su mando para la ranura nueva.
            if let Some(state) = self.guests.get_mut(&guest) {
                state.pad = None;
            }
        } else if old.gamepad_raw != permissions.gamepad_raw
            || old.keyboard_mouse != permissions.keyboard_mouse
        {
            self.apply_pad_mode(guest);
        }
    }

    /// Datos de mandos tal cual de `guest` (ya autenticados).
    fn pads(&mut self, guest: GuestId, pads: Vec<PadData>) {
        for pad in pads {
            let Some(state) = self.guests.get_mut(&guest) else {
                return;
            };
            if state.permissions.gamepad_slot != Some(pad.slot()) {
                state.dropped += 1;
                self.totals.1 += 1;
                continue;
            }
            match pad {
                PadData::Device { slot, device } => {
                    if state.pad.as_ref().is_some_and(|pad| pad.active)
                        && let Err(err) = self.sink.disconnect_raw(slot)
                    {
                        self.warn(&err);
                    }
                    let state = self.guests.get_mut(&guest).expect("invitado");
                    state.pad = Some(PadState {
                        slot,
                        device,
                        active: false,
                        note: None,
                    });
                    state.dirty = true;
                    self.apply_pad_mode(guest);
                }
                PadData::Report { slot, data } => {
                    if self.paused || !state.pad.as_ref().is_some_and(PadState::guest_sends_raw) {
                        state.dropped += 1;
                        self.totals.1 += 1;
                        continue;
                    }
                    state.applied += 1;
                    self.totals.0 += 1;
                    if let Err(err) = self.sink.raw_report(slot, &data) {
                        self.warn(&err);
                    }
                }
            }
        }
    }

    /// Pone el mando de `guest` tal cual o convertido según su permiso, su declaración y la
    /// pausa, y le avisa del modo.
    fn apply_pad_mode(&mut self, guest: GuestId) {
        let paused = self.paused;
        let Some(state) = self.guests.get_mut(&guest) else {
            return;
        };
        let permissions = state.permissions;
        let Some(pad) = state.pad.as_mut() else {
            return;
        };
        let check = if permissions.gamepad_raw {
            check_device(&pad.device, &permissions)
        } else {
            Err(String::new())
        };
        let mut error = None;
        match (check, pad.active) {
            (Ok(()), false) if !paused => match self.sink.connect_raw(pad.slot, &pad.device) {
                Ok(()) => {
                    pad.active = true;
                    pad.note = None;
                }
                Err(err) => {
                    pad.note = Some(err.to_string());
                    error = Some(err);
                }
            },
            (Ok(()), true) if !paused => {}
            (check, active) => {
                if active {
                    if let Err(err) = self.sink.disconnect_raw(pad.slot) {
                        error = Some(err);
                    }
                    pad.active = false;
                }
                pad.note = match check {
                    Err(reason) if !reason.is_empty() => Some(reason),
                    _ => None,
                };
            }
        }
        let (slot, raw) = (pad.slot, pad.guest_sends_raw());
        state.dirty = true;
        if let Some(err) = error {
            self.warn(&err);
        }
        self.command(Command::GamepadMode { guest, slot, raw });
    }

    /// El invitado desconectó el mando de `slot`: vuelve el convertido.
    fn forget_pad(&mut self, guest: GuestId, slot: u8) {
        let Some(state) = self.guests.get_mut(&guest) else {
            return;
        };
        let Some(pad) = state.pad.take_if(|pad| pad.slot == slot) else {
            return;
        };
        state.dirty = true;
        if pad.active
            && let Err(err) = self.sink.disconnect_raw(slot)
        {
            self.warn(&err);
        }
    }

    fn any_raw(&self) -> bool {
        self.guests
            .values()
            .any(|state| state.pad.as_ref().is_some_and(|pad| pad.active))
    }

    /// Vibración y luces que el sistema mandó a los mandos tal cual, a su invitado.
    fn forward_outputs(&mut self) {
        for (slot, data) in self.sink.take_outputs() {
            let owner = self.guests.iter().find_map(|(guest, state)| {
                state
                    .pad
                    .as_ref()
                    .filter(|pad| pad.active && pad.slot == slot)
                    .map(|_| *guest)
            });
            if let Some(guest) = owner {
                self.command(Command::GamepadOutput { guest, slot, data });
            }
        }
    }

    fn command(&self, command: Command) {
        if let Some(commands) = lock(&self.core.commands).as_ref() {
            let _ = commands.send(command);
        }
    }

    fn gone(&mut self, guest: GuestId) {
        let Some(state) = self.guests.remove(&guest) else {
            return;
        };
        if let Err(err) = self.sink.release_all(guest) {
            self.warn(&err);
        }
        if let Some(slot) = state.permissions.gamepad_slot
            && let Err(err) = self.sink.release_gamepad(slot)
        {
            self.warn(&err);
        }
    }

    /// Pasa los contadores y la latencia de los invitados que cambiaron a su `GuestStatus`.
    fn publish(&mut self) {
        let dirty: Vec<(GuestId, InputLatency, Option<GuestPad>)> = self
            .guests
            .iter_mut()
            .filter(|(_, state)| state.dirty)
            .map(|(guest, state)| {
                state.dirty = false;
                (
                    *guest,
                    InputLatency::from_samples(
                        state.latencies.iter().copied().collect(),
                        state.applied,
                        state.dropped,
                    ),
                    state.pad.as_ref().map(PadState::status),
                )
            })
            .collect();
        self.core
            .ledger
            .set_input_counts(self.totals.0, self.totals.1);
        if dirty.is_empty() {
            return;
        }
        let mut guests = lock(&self.core.guests);
        for (guest, latency, pad) in dirty {
            if let Some(status) = guests.get_mut(&guest) {
                status.input = latency;
                status.gamepad = pad;
            }
        }
    }
}

// --- Invitado ----------------------------------------------------------------------------------

#[derive(Default)]
struct GuestInputInner {
    allowed: Mutex<Allowed>,
    frame: Mutex<Option<(u32, u32)>>,
    report: Mutex<Option<InputLatency>>,
    sender: Mutex<Option<InputSender>>,
    stopped: AtomicBool,
    /// Ranuras cuyo mando el anfitrión tiene tal cual (`GamepadMode`).
    pad_modes: Mutex<BTreeMap<u8, bool>>,
    /// Informes de salida del anfitrión para el mando (`GamepadOutput`), los más recientes.
    pad_outputs: Mutex<VecDeque<(u8, Vec<u8>)>>,
}

/// Informes de salida del anfitrión que se guardan como mucho hasta que el hilo de mandos los
/// recoge.
const MAX_PAD_OUTPUTS: usize = 64;

/// Entrada del invitado: lo que la ventana (o el guion) necesita de la sesión.
#[derive(Clone, Default)]
pub struct GuestInput {
    inner: Arc<GuestInputInner>,
}

impl std::fmt::Debug for GuestInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuestInput")
            .field("allowed", &self.allowed())
            .field("ready", &self.ready())
            .finish_non_exhaustive()
    }
}

impl GuestInput {
    /// Envía un evento al anfitrión (si la entrada ya está lista; si no, se pierde).
    pub fn send(&self, event: InputEvent) {
        if let Some(sender) = lock(&self.inner.sender).as_ref() {
            sender.send(event);
        }
    }

    /// Si el canal de entrada está abierto.
    pub fn ready(&self) -> bool {
        lock(&self.inner.sender).is_some()
    }

    /// Lo que el anfitrión permite ahora.
    pub fn allowed(&self) -> Allowed {
        *lock(&self.inner.allowed)
    }

    /// Tamaño del fotograma del anfitrión.
    pub fn host_frame_size(&self) -> Option<(u32, u32)> {
        *lock(&self.inner.frame)
    }

    /// Latencia de entrada y contadores que informa el anfitrión.
    pub fn report(&self) -> Option<InputLatency> {
        *lock(&self.inner.report)
    }

    /// Contadores del emisor local.
    pub fn sender_stats(&self) -> Option<SenderSnapshot> {
        lock(&self.inner.sender).as_ref().map(InputSender::stats)
    }

    pub(crate) fn set_allowed(&self, allowed: Allowed) {
        *lock(&self.inner.allowed) = allowed;
    }

    pub(crate) fn set_frame(&self, width: u32, height: u32) {
        *lock(&self.inner.frame) = Some((width, height));
    }

    pub(crate) fn set_report(&self, report: InputLatency) {
        *lock(&self.inner.report) = Some(report);
    }

    pub(crate) fn set_sender(&self, sender: Option<InputSender>) {
        let old = std::mem::replace(&mut *lock(&self.inner.sender), sender);
        drop(old);
        // Conexión nueva: el anfitrión vuelve a decir el modo de cada mando.
        lock(&self.inner.pad_modes).clear();
        lock(&self.inner.pad_outputs).clear();
    }

    /// Envía datos del mando tal cual al anfitrión (si la entrada ya está lista).
    pub fn send_pad(&self, data: PadData) {
        if let Some(sender) = lock(&self.inner.sender).as_ref() {
            sender.send_pad(data);
        }
    }

    pub(crate) fn set_pad_mode(&self, slot: u8, raw: bool) {
        lock(&self.inner.pad_modes).insert(slot, raw);
    }

    pub(crate) fn push_pad_output(&self, slot: u8, data: Vec<u8>) {
        if data.is_empty() || data.len() > hid::MAX_REPORT_BYTES {
            return;
        }
        let mut outputs = lock(&self.inner.pad_outputs);
        if outputs.len() >= MAX_PAD_OUTPUTS {
            outputs.pop_front();
        }
        outputs.push_back((slot, data));
    }

    pub(crate) fn stop(&self) {
        self.inner.stopped.store(true, Ordering::Release);
        self.set_sender(None);
    }
}

impl PadLink for GuestInput {
    fn event(&self, event: InputEvent) {
        GuestInput::send(self, event);
    }

    fn pad(&self, data: PadData) {
        self.send_pad(data);
    }

    fn raw(&self, slot: u8) -> bool {
        lock(&self.inner.pad_modes)
            .get(&slot)
            .copied()
            .unwrap_or(false)
    }

    fn outputs(&self) -> Vec<(u8, Vec<u8>)> {
        lock(&self.inner.pad_outputs).drain(..).collect()
    }
}

impl ScriptTarget for GuestInput {
    fn send(&self, event: InputEvent) {
        GuestInput::send(self, event);
    }

    fn allowed(&self) -> Allowed {
        GuestInput::allowed(self)
    }

    fn stopped(&self) -> bool {
        self.inner.stopped.load(Ordering::Acquire)
    }
}

/// La conexión QUIC del invitado como [`InputLink`].
struct QuicLink {
    conn: PeerConnection,
    stream: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    receiver: Arc<MediaReceiver>,
}

impl InputLink for QuicLink {
    fn send_datagram(&self, datagram: Vec<u8>) -> Result<(), String> {
        self.conn
            .send_datagram(Bytes::from(datagram))
            .map_err(|err| err.to_string())
    }

    fn send_stream(&self, frame: Vec<u8>) -> Result<(), String> {
        self.stream
            .send(frame)
            .map_err(|_| "el stream de entrada se cerró".to_owned())
    }

    fn remote_clock_us(&self, at: Instant) -> Option<u64> {
        self.receiver.remote_us(at)
    }
}

/// Invitado: con la conexión abierta, abre el stream de entrada y arranca el emisor.
pub(crate) async fn open_guest_input(
    core: &Arc<Core>,
    connection: &PeerConnection,
    receiver: &Arc<MediaReceiver>,
) -> Result<(), String> {
    let deadline = Instant::now() + KEY_WAIT;
    let secret = loop {
        if let Some(secret) = lock(&core.input_keys)
            .get(&coroom_transport::proto::signal::HOST_PEER_ID)
            .cloned()
        {
            break secret;
        }
        if Instant::now() >= deadline {
            return Err(
                "sin clave pareja a pareja con el anfitrión: no se enviará entrada".to_owned(),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let cipher = InputCipher::from_pairwise(&secret, Role::Guest).map_err(|err| err.to_string())?;
    let (mut send, recv) = connection
        .open_input()
        .await
        .map_err(|err| format!("no se pudo abrir el stream de entrada: {err}"))?;
    send.write_all(&stream_preamble(cipher.key_id()))
        .await
        .map_err(|err| format!("no se pudo abrir el stream de entrada: {err}"))?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        let _recv = recv;
        while let Some(frame) = rx.recv().await {
            if send.write_all(&frame).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
    });
    let link = QuicLink {
        conn: connection.clone(),
        stream: tx,
        receiver: Arc::clone(receiver),
    };
    let sender = InputSender::spawn(Arc::new(link), Arc::new(Mutex::new(cipher)))
        .map_err(|err| err.to_string())?;
    core.guest_input.set_sender(Some(sender));
    Ok(())
}
