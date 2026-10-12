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
use coroom_input::inject::{EisConnection, EisProvider};
use coroom_input::script::ScriptTarget;
use coroom_input::send::{InputLink, InputSender, SenderSnapshot};
use coroom_input::{EventClass, InputError, InputEvent, InputSink};
use coroom_transport::PeerConnection;
use coroom_transport::crypto::hpke::PairwiseSecret;
use coroom_transport::crypto::input::{Channel, InputCipher, Role};
use coroom_transport::media::{MediaReceiver, MediaSender};
use coroom_transport::proto::input::{
    InputMessage, InputPacket, STREAM_PREAMBLE_LEN, StreamFrames, parse_datagram,
    parse_stream_preamble, seq_after, stream_preamble,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use tracing::debug;

use super::{Core, lock};
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
/// la sesión del portal es una sola para las dos cosas.
pub fn eis_provider(share: coroom_capture::RemoteDesktopShare) -> EisProvider {
    Box::new(move || {
        share.wait(Duration::from_secs(300)).map(|socket| {
            // Fuera de Unix no hay descriptor que pasar (la inyección por EI es de Linux).
            #[cfg(not(unix))]
            let _ = socket;
            EisConnection {
                #[cfg(unix)]
                fd: socket.fd,
            }
        })
    })
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
            });
        }
    }
}

/// Estado de entrada de un invitado en el hilo `inject`.
#[derive(Default)]
struct GuestState {
    permissions: Permissions,
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
            match rx.recv_timeout(Duration::from_millis(100)) {
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
                latency_us,
            } => self.packet(guest, datagram, seq, &events, latency_us),
            Job::Permissions { guest, permissions } => self.permissions(guest, permissions),
            Job::Paused(paused) => {
                if paused && !self.paused {
                    let guests: Vec<GuestId> = self.guests.keys().copied().collect();
                    for guest in guests {
                        if let Err(err) = self.sink.release_all(guest) {
                            self.warn(&err);
                        }
                    }
                }
                self.paused = paused;
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
        let dirty: Vec<(GuestId, InputLatency)> = self
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
        for (guest, latency) in dirty {
            if let Some(status) = guests.get_mut(&guest) {
                status.input = latency;
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
}

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
    }

    pub(crate) fn stop(&self) {
        self.inner.stopped.store(true, Ordering::Release);
        self.set_sender(None);
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
