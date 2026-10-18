//! Entrada del invitado en la sesión remota.
//!
//! **Anfitrión** ([`HostInput`]): por cada invitado con secreto pareja a pareja guarda un
//! [`InputCipher`] indexado por su `key_id`. Por cada conexión (la directa de cada invitado y la del
//! relay, que trae a varios) hay dos tareas: una lee los datagramas de la pista `Input` (separados
//! de los de medios por la propia conexión) y otra acepta los streams de entrada que abre el
//! invitado (solo declaraciones de mandos). Las dos abren el sellado (que autentica al invitado:
//! sin su clave no abre) y pasan lo que llega al hilo `inject`, el único que toca el [`InputSink`].
//! Ese hilo lleva el estado autoritativo de permisos y pausa, así que todo pasa en orden: lo no
//! permitido se descarta y se cuenta, la pausa descarta todo y suelta lo pulsado (`release_all`)
//! (y lo mismo mientras el anfitrión no comparte nada: «Dejar de compartir» o una sala solo de voz),
//! al quitar o cambiar la ranura se destruye o crea el mando, y al expulsar o perder a un invitado
//! se suelta lo suyo y se libera su ranura.
//!
//! Cada paquete trae los eventos y el estado completo del invitado ([`InputState`]). Con la
//! secuencia seguida se aplican los eventos en orden; si falta alguna (se perdió un datagrama), el
//! hilo compara su vista del invitado con el estado que llega y aplica la diferencia
//! ([`InputState::diff`]): nada se queda pulsado ni sin pulsar. Lo más antiguo que lo último
//! aplicado se descarta.
//!
//! Los eventos permitidos de un paquete van al sumidero **agrupados** por clase (teclado y ratón,
//! o un mando) y en orden ([`InputSink::inject_batch`]): el sumidero los escribe en una sola trama
//! (uinput, EI) o llamada (`SendInput`), así que un stick con `X` e `Y` cambiados llega al juego
//! como una muestra.
//!
//! **Vigilancia**: el invitado repite su estado cada `KEEPALIVE_PRESSED` mientras tiene algo
//! pulsado; si de un invitado no llega nada en [`STALE_AFTER`] y su vista tiene algo pulsado (se
//! perdieron una liberación, sus repeticiones y los *keepalives*, o el enlace murió), se suelta lo
//! suyo sin esperar al tiempo de inactividad de QUIC, y el siguiente paquete que llegue se
//! reconcilia con la vista aunque traiga la secuencia seguida.
//!
//! **Latencia por etapas**: con `queued_us` del paquete (captura → envío en el invitado),
//! `recibido − enviado` en el reloj del `MediaSender` de ese invitado (el que contesta a sus
//! `Ping`) y el tiempo de inyección medido aquí, cada invitado tiene su captura → inyectado
//! desglosado en su `GuestStatus`, en el resumen y en el `InputReport` que se le devuelve. Se
//! publican cada [`PUBLISH_PERIOD`] (lo que tarda `rate_loop` en leerlas), no con cada paquete:
//! ordenar las muestras no entra en la etapa de inyección.
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
use coroom_input::{EventClass, InputError, InputEvent, InputSink, Rumble};
use coroom_transport::PeerConnection;
use coroom_transport::crypto::hpke::PairwiseSecret;
use coroom_transport::crypto::input::{Channel, InputCipher, Role};
use coroom_transport::media::{MediaReceiver, MediaSender};
use coroom_transport::proto::hid;
use coroom_transport::proto::input::{
    GamepadDevice, GamepadTransport, InputMessage, InputPacket, InputState, PadData, STALE_AFTER,
    STREAM_PREAMBLE_LEN, StreamFrames, parse_datagram, parse_stream_preamble, seq_after,
    stream_preamble,
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

/// Cada cuánto publica el hilo `inject` la latencia y los contadores de cada invitado en su
/// `GuestStatus`: lo mismo que tarda `rate_loop` en leerlos (`RATE_PERIOD`). Lo demás (ranura,
/// permisos, modo del mando) se publica al momento.
const PUBLISH_PERIOD: Duration = Duration::from_millis(100);

/// Con mandos asignados, cada cuánto se recogen del sumidero sus informes de salida (vibración,
/// luces) y el estado de sus motores para el invitado (en Linux, es también lo que espera como
/// mucho un juego que sube un efecto de vibración al mando uinput).
const OUTPUTS_PERIOD: Duration = Duration::from_millis(5);

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
        coroom_capture::SharedArea::Monitor { x, y, mapping_id } => {
            PointerArea::Monitor { x, y, mapping_id }
        }
        coroom_capture::SharedArea::Window { width, height } => {
            PointerArea::Window { width, height }
        }
    }
}

// --- Anfitrión ---------------------------------------------------------------------------------

/// Trabajo del hilo `inject`.
pub(crate) enum Job {
    /// Paquete de un invitado ya autenticado.
    Packet {
        guest: GuestId,
        seq: u32,
        /// Cuándo llegó el datagrama.
        received: Instant,
        /// Captura → envío en el invitado, en µs.
        queued_us: u16,
        /// Captura → recibido, en µs (con el reloj sincronizado), si se conoce.
        network_us: Option<u64>,
        events: Vec<InputEvent>,
        /// [`InputPacket::text_seq`](coroom_transport::proto::input::InputPacket::text_seq).
        text_seq: u16,
        state: InputState,
        pads: Vec<PadData>,
    },
    /// Declaraciones de mandos tal cual de un invitado (por el stream).
    Declare { guest: GuestId, pads: Vec<PadData> },
    /// Permisos nuevos.
    Permissions {
        guest: GuestId,
        permissions: Permissions,
    },
    /// Pausa global.
    Paused(bool),
    /// Conexión EIS de la captura nueva (cambio de fuente con la sala abierta, Linux).
    Eis(EisProvider),
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
            seq: packet.seq,
            received,
            queued_us: packet.queued_us,
            network_us: latency_us(&clock, received, packet.sent_us),
            events: packet.events,
            text_seq: packet.text_seq,
            state: packet.state,
            pads: packet.pads,
        });
    }
}

async fn stream_acceptor(
    input: Arc<HostInput>,
    core: Arc<Core>,
    conn: PeerConnection,
    only: Option<GuestId>,
) {
    while let Ok((send, recv)) = conn.accept_input().await {
        tokio::spawn(stream_reader(
            Arc::clone(&input),
            Arc::clone(&core),
            send,
            recv,
            only,
        ));
    }
}

async fn stream_reader(
    input: Arc<HostInput>,
    core: Arc<Core>,
    _send: coroom_transport::SendStream,
    mut recv: coroom_transport::RecvStream,
    only: Option<GuestId>,
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
            input.job(Job::Declare {
                guest,
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

/// Muestras de una etapa, las últimas [`LATENCY_WINDOW`].
#[derive(Default)]
struct Samples(VecDeque<u64>);

impl Samples {
    fn push(&mut self, us: u64) {
        self.0.push_back(us);
        while self.0.len() > LATENCY_WINDOW {
            self.0.pop_front();
        }
    }

    fn p50(&self) -> u64 {
        if self.0.is_empty() {
            return 0;
        }
        let mut values: Vec<u64> = self.0.iter().copied().collect();
        let middle = values.len() / 2;
        *values.select_nth_unstable(middle).1
    }
}

/// Estado de entrada de un invitado en el hilo `inject`.
#[derive(Default)]
struct GuestState {
    permissions: Permissions,
    pad: Option<PadState>,
    /// Última secuencia aplicada.
    last_seq: Option<u32>,
    /// Los textos ya tecleados.
    texts: TextOrder,
    /// Lo que el invitado tiene pulsado y acumulado, según lo aplicado hasta ahora.
    view: InputState,
    applied: u64,
    dropped: u64,
    /// Huecos de secuencia reconciliados con el estado.
    repaired: u64,
    /// Captura → inyectado.
    latencies: Samples,
    queue: Samples,
    network: Samples,
    inject: Samples,
    /// Cambió algo que se publica al momento (permisos, ranura, modo del mando).
    dirty: bool,
    /// Hay muestras o contadores nuevos desde la última publicación periódica.
    stats_changed: bool,
    /// Cuándo llegó su último paquete autenticado (para [`STALE_AFTER`]).
    last_packet: Option<Instant>,
    /// Se soltó lo suyo por silencio: el siguiente paquete se reconcilia con la vista.
    stale: bool,
}

/// Cuándo y cómo llegó un paquete de entrada.
#[derive(Debug, Clone, Copy)]
struct PacketTiming {
    seq: u32,
    /// Cuándo llegó el datagrama.
    received: Instant,
    /// Captura → envío en el invitado, en µs.
    queued_us: u16,
    /// Captura → recibido, en µs, si se conoce.
    network_us: Option<u64>,
}

/// Qué textos ([`InputEvent::Text`]) de un invitado ya se teclearon: el número de orden del
/// siguiente. Cada paquete trae los textos que el invitado aún repite (los más antiguos delante,
/// con números consecutivos desde [`InputPacket::text_seq`]) y los nuevos detrás; los paquetes se
/// aplican en orden (los atrasados se descartan enteros), así que un texto no puede llegar antes
/// que uno anterior que aún se repita: no hay que guardar ninguno a la espera.
///
/// [`InputPacket::text_seq`]: coroom_transport::proto::input::InputPacket::text_seq
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TextOrder {
    next: Option<u16>,
}

impl TextOrder {
    /// De los `count` textos de un paquete (el primero con el número `first`), cuántos del
    /// principio ya se teclearon (los demás son nuevos y van en orden) y cuántos se perdieron
    /// sin remedio entre el último tecleado y `first` (dejaron de repetirse antes de que llegara
    /// ninguna copia).
    fn accept(&mut self, first: u16, count: usize) -> (usize, u16) {
        if count == 0 {
            return (0, 0);
        }
        let after = first.wrapping_add(count as u16);
        let Some(next) = self.next else {
            self.next = Some(after);
            return (0, 0);
        };
        let ahead = first.wrapping_sub(next) as i16;
        if ahead > 0 {
            self.next = Some(after);
            return (0, ahead as u16);
        }
        let done = usize::from(ahead.unsigned_abs()).min(count);
        if done < count {
            self.next = Some(after);
        }
        (done, 0)
    }
}

/// El hilo `inject`: único dueño del sumidero.
struct Worker {
    core: Arc<Core>,
    sink: Box<dyn InputSink>,
    guests: BTreeMap<GuestId, GuestState>,
    /// No se inyecta nada: la pausa del anfitrión ([`Job::Paused`]) o no se comparte nada.
    paused: bool,
    /// La pausa que pidió el anfitrión.
    user_paused: bool,
    /// Si se comparte algo (lo último visto de `Shared::sharing`): sin nada compartido, la entrada
    /// de los invitados no llega al equipo (ni teclado ni ratón ni mandos), como en pausa.
    sharing: bool,
    warned: Vec<String>,
    totals: (u64, u64),
    /// Última publicación periódica y última recogida de salidas de mandos.
    last_publish: Instant,
    last_outputs: Instant,
    /// Grupo de eventos de la misma clase en curso (reutilizado entre paquetes).
    run: Vec<InputEvent>,
}

impl Worker {
    fn new(core: Arc<Core>, sink: Box<dyn InputSink>) -> Self {
        let now = Instant::now();
        Self {
            core,
            sink,
            guests: BTreeMap::new(),
            paused: false,
            user_paused: false,
            sharing: true,
            warned: Vec::new(),
            totals: (0, 0),
            last_publish: now,
            last_outputs: now,
            run: Vec::with_capacity(64),
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
            let now = Instant::now();
            // Trabajo periódico del sumidero (repetición de teclas en Windows): se hace y acota la
            // espera.
            let tick = self.sink.tick(now);
            // Con mandos, su vibración sale enseguida aunque no llegue nada (y en Linux los juegos
            // que suben efectos esperan respuesta).
            let mut wait = if self.any_pad() {
                OUTPUTS_PERIOD
            } else {
                PUBLISH_PERIOD
            };
            if let Some(tick) = tick {
                wait = wait.min(tick.saturating_duration_since(now));
            }
            match rx.recv_timeout(wait) {
                Ok(job) => {
                    self.handle(job);
                    for _ in 0..256 {
                        match rx.try_recv() {
                            Ok(job) => self.handle(job),
                            Err(_) => break,
                        }
                    }
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
            self.housekeeping(Instant::now());
        }
        // Al terminar no puede quedar nada pulsado ni ningún mando creado.
        let guests: Vec<GuestId> = self.guests.keys().copied().collect();
        for guest in guests {
            self.gone(guest);
        }
        self.publish(true);
    }

    /// Lo que no es un trabajo: la vigilancia de invitados callados, las salidas y la vibración
    /// de los mandos (cada [`OUTPUTS_PERIOD`]) y la publicación de estadísticas (cada
    /// [`PUBLISH_PERIOD`]).
    fn housekeeping(&mut self, now: Instant) {
        self.follow_sharing();
        self.expire_stale(now);
        if self.any_pad() && now.saturating_duration_since(self.last_outputs) >= OUTPUTS_PERIOD {
            self.last_outputs = now;
            self.forward_outputs();
            self.forward_rumble();
        }
        let periodic = now.saturating_duration_since(self.last_publish) >= PUBLISH_PERIOD;
        if periodic {
            self.last_publish = now;
        }
        self.publish(periodic);
    }

    /// Suelta lo de los invitados de los que no llega nada desde hace [`STALE_AFTER`] y que,
    /// según la vista, tienen algo pulsado. Su siguiente paquete se reconcilia con la vista.
    fn expire_stale(&mut self, now: Instant) {
        if self.paused {
            return;
        }
        let mut errors = Vec::new();
        for (guest, state) in &mut self.guests {
            let Some(last) = state.last_packet else {
                continue;
            };
            if state.stale
                || !state.view.any_pressed()
                || now.saturating_duration_since(last) < STALE_AFTER
            {
                continue;
            }
            debug!(
                guest = guest.0,
                "sin entrada del invitado en {STALE_AFTER:?}: se suelta lo suyo"
            );
            state.stale = true;
            state.view.release_all();
            state.stats_changed = true;
            if let Err(err) = self.sink.release_all(*guest) {
                errors.push(err);
            }
        }
        for err in errors {
            self.warn(&err);
        }
    }

    fn handle(&mut self, job: Job) {
        // Antes de cada trabajo: un paquete que llega ya sin nada compartido no se inyecta.
        self.follow_sharing();
        match job {
            Job::Packet {
                guest,
                seq,
                received,
                queued_us,
                network_us,
                events,
                text_seq,
                state,
                pads,
            } => {
                let timing = PacketTiming {
                    seq,
                    received,
                    queued_us,
                    network_us,
                };
                if self.packet(guest, timing, events, text_seq, state) {
                    self.pads(guest, pads);
                }
            }
            Job::Declare { guest, pads } => self.pads(guest, pads),
            Job::Permissions { guest, permissions } => self.permissions(guest, permissions),
            Job::Eis(eis) => self.sink.replace_eis(eis),
            Job::Paused(paused) => {
                self.user_paused = paused;
                self.update_paused();
            }
            Job::GuestGone(guest) => self.gone(guest),
        }
    }

    /// Sigue a la fuente: al dejar de compartir se deja de inyectar (y se suelta todo) y al
    /// volver a compartir se reanuda.
    fn follow_sharing(&mut self) {
        let sharing = self.core.shared.sharing();
        if sharing != self.sharing {
            self.sharing = sharing;
            debug!(sharing, "entrada de los invitados según lo que se comparte");
            self.update_paused();
        }
    }

    /// Aplica la pausa que resulta de la del anfitrión y de lo que se comparte: al entrar se
    /// suelta lo pulsado de todos; al entrar o salir, los mandos tal cual se quitan o vuelven.
    fn update_paused(&mut self) {
        let paused = self.user_paused || !self.sharing;
        let was = self.paused;
        if paused == was {
            return;
        }
        self.paused = paused;
        let guests: Vec<GuestId> = self.guests.keys().copied().collect();
        if paused {
            for guest in &guests {
                if let Err(err) = self.sink.release_all(*guest) {
                    self.warn(&err);
                }
            }
        }
        // Un mando tal cual no tiene un informe «en reposo» que inyectar: en pausa se quita (el
        // sistema suelta lo que tuviera pulsado) y vuelve al reanudar.
        for guest in guests {
            self.apply_pad_mode(guest);
        }
    }

    /// Aplica un paquete; devuelve `false` si se descartó entero (antiguo o en pausa).
    fn packet(
        &mut self,
        guest: GuestId,
        timing: PacketTiming,
        mut events: Vec<InputEvent>,
        text_seq: u16,
        state: InputState,
    ) -> bool {
        let PacketTiming {
            seq,
            received,
            queued_us,
            network_us,
        } = timing;
        let paused = self.paused;
        let entry = self.guests.entry(guest).or_default();
        entry.stats_changed = true;
        entry.last_packet = Some(received);
        // Lo más antiguo que lo último aplicado (o repetido) ya no sirve.
        if entry.last_seq.is_some_and(|last| !seq_after(seq, last)) {
            entry.dropped += events.len() as u64;
            self.totals.1 += events.len() as u64;
            return false;
        }
        // Tras soltar lo suyo por silencio, la vista ya no es la del invitado: se reconcilia
        // aunque la secuencia venga seguida.
        let gap = entry.stale
            || entry
                .last_seq
                .is_some_and(|last| last.wrapping_add(1) != seq);
        entry.stale = false;
        entry.last_seq = Some(seq);
        // Los textos que el invitado repite y ya se teclearon fuera (también en pausa: al
        // reanudar no se teclea lo que escribió entretanto).
        let texts = events
            .iter()
            .filter(|event| matches!(event, InputEvent::Text { .. }))
            .count();
        let (mut repeated, lost) = entry.texts.accept(text_seq, texts);
        if lost > 0 {
            debug!(
                ?guest,
                lost, "textos del invitado perdidos: ya no se repetían cuando volvió a llegar uno"
            );
        }
        if repeated > 0 {
            events.retain(|event| {
                let skip = repeated > 0 && matches!(event, InputEvent::Text { .. });
                repeated -= usize::from(skip);
                !skip
            });
        }
        if paused {
            entry.dropped += events.len() as u64;
            self.totals.1 += events.len() as u64;
            // En pausa no se inyecta, pero la vista sigue al invitado: al reanudar no se
            // reconstruye lo que pulsó durante la pausa.
            entry.view = state;
            return false;
        }
        // Con hueco, los eventos de este paquete presuponen los perdidos: se aplica la diferencia
        // entre la vista y el estado que llega, que los incluye. El texto no está en el estado: el
        // de este paquete va detrás de la diferencia (el de los perdidos, perdido queda).
        let events = if gap {
            entry.repaired += 1;
            let mut repaired = entry.view.diff(&state);
            repaired.extend(
                events
                    .iter()
                    .filter(|event| matches!(event, InputEvent::Text { .. })),
            );
            repaired
        } else {
            events
        };
        let permissions = entry.permissions;
        let mut errors = Vec::new();
        let (mut applied, mut dropped) = (0u64, 0u64);
        // Los permitidos van al sumidero agrupados por clase y en orden; `ReleaseAll` y la
        // desconexión de un mando cierran el grupo en curso antes de aplicarse.
        let mut run = std::mem::take(&mut self.run);
        run.clear();
        let mut run_class: Option<EventClass> = None;
        for event in &events {
            let class = event.class();
            let allowed = match class {
                EventClass::KeyboardMouse => permissions.keyboard_mouse,
                EventClass::Gamepad(slot) => permissions.gamepad_slot == Some(slot),
                EventClass::Control => true,
            };
            if !allowed {
                dropped += 1;
                continue;
            }
            match event {
                InputEvent::ReleaseAll => {
                    self.flush_run(guest, &mut run, &mut errors, &mut applied, &mut dropped);
                    match self.sink.release_all(guest) {
                        Ok(()) => applied += 1,
                        Err(err) => {
                            dropped += 1;
                            errors.push(err);
                        }
                    }
                    continue;
                }
                InputEvent::GamepadDisconnected { slot } => {
                    self.flush_run(guest, &mut run, &mut errors, &mut applied, &mut dropped);
                    self.forget_pad(guest, *slot);
                }
                _ => {}
            }
            if run_class != Some(class) {
                self.flush_run(guest, &mut run, &mut errors, &mut applied, &mut dropped);
                run_class = Some(class);
            }
            run.push(*event);
        }
        self.flush_run(guest, &mut run, &mut errors, &mut applied, &mut dropped);
        self.run = run;
        let injected = Instant::now();
        if let Some(entry) = self.guests.get_mut(&guest) {
            entry.view = state;
            entry.applied += applied;
            entry.dropped += dropped;
            if !events.is_empty() {
                let inject_us = injected.saturating_duration_since(received).as_micros() as u64;
                entry.inject.push(inject_us);
                entry.queue.push(u64::from(queued_us));
                if let Some(network_us) = network_us {
                    entry.network.push(network_us);
                    let total = network_us + inject_us;
                    entry.latencies.push(total);
                    self.core.ledger.record_input_latency(
                        total,
                        u64::from(queued_us),
                        network_us,
                        inject_us,
                    );
                }
            }
        }
        self.totals.0 += applied;
        self.totals.1 += dropped;
        for err in errors {
            self.warn(&err);
        }
        true
    }

    /// Manda al sumidero el grupo en curso (si hay) y lo vacía, contando lo aplicado.
    fn flush_run(
        &mut self,
        guest: GuestId,
        run: &mut Vec<InputEvent>,
        errors: &mut Vec<InputError>,
        applied: &mut u64,
        dropped: &mut u64,
    ) {
        if run.is_empty() {
            return;
        }
        let done = self.sink.inject_batch(run, guest, errors);
        *applied += done as u64;
        *dropped += (run.len() - done) as u64;
        run.clear();
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

    /// Si algún invitado tiene ranura de mando (con su mando virtual en el anfitrión).
    fn any_pad(&self) -> bool {
        self.guests
            .values()
            .any(|state| state.permissions.gamepad_slot.is_some())
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

    /// Lo que los juegos pidieron a los motores de los mandos convertidos y de Xbox por cable, al
    /// invitado de su ranura (al de ninguna, nada). Va también en pausa: es el juego, no el
    /// invitado, quien la pide, y parar los motores tiene que llegar siempre.
    fn forward_rumble(&mut self) {
        for (slot, rumble) in self.sink.take_rumble() {
            let owner = self.guests.iter().find_map(|(guest, state)| {
                (state.permissions.gamepad_slot == Some(slot)).then_some(*guest)
            });
            if let Some(guest) = owner {
                self.command(Command::GamepadRumble {
                    guest,
                    slot,
                    rumble,
                });
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

    /// Pasa a los `GuestStatus` lo de los invitados que cambiaron algo inmediato (ranura,
    /// permisos, modo del mando) y, si `periodic`, también la latencia y los contadores de los que
    /// tienen muestras nuevas (ordenar [`LATENCY_WINDOW`] muestras por etapa no se hace por
    /// paquete).
    fn publish(&mut self, periodic: bool) {
        let mut updates: Vec<(GuestId, InputLatency, Option<GuestPad>)> = Vec::new();
        for (guest, state) in &mut self.guests {
            if !(state.dirty || (periodic && state.stats_changed)) {
                continue;
            }
            state.dirty = false;
            state.stats_changed = false;
            updates.push((
                *guest,
                InputLatency::from_samples(
                    state.latencies.0.iter().copied().collect(),
                    state.applied,
                    state.dropped,
                )
                .with_stages(
                    state.queue.p50(),
                    state.network.p50(),
                    state.inject.p50(),
                    state.repaired,
                ),
                state.pad.as_ref().map(PadState::status),
            ));
        }
        if periodic {
            self.core
                .ledger
                .set_input_counts(self.totals.0, self.totals.1);
        }
        if updates.is_empty() {
            return;
        }
        let mut guests = lock(&self.core.guests);
        for (guest, latency, pad) in updates {
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
    /// Lo que el anfitrión pide a los motores de cada ranura (`GamepadRumble`), el último.
    pad_rumble: Mutex<BTreeMap<u8, Rumble>>,
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
        // La vibración de una ranura que ya no es suya no vale (el anfitrión quitó ese mando).
        lock(&self.inner.pad_rumble).retain(|slot, _| allowed.gamepad_slot == Some(*slot));
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
        // Y los motores se paran hasta que diga otra cosa (también al terminar la sesión).
        lock(&self.inner.pad_rumble).clear();
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

    pub(crate) fn set_pad_rumble(&self, slot: u8, rumble: Rumble) {
        let mut motors = lock(&self.inner.pad_rumble);
        if rumble.is_on() {
            motors.insert(slot, rumble);
        } else {
            motors.remove(&slot);
        }
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

    fn event_at(&self, event: InputEvent, at: Instant) {
        if let Some(sender) = lock(&self.inner.sender).as_ref() {
            sender.send_at(event, at);
        }
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

    fn rumble(&self, slot: u8) -> Rumble {
        lock(&self.inner.pad_rumble)
            .get(&slot)
            .copied()
            .unwrap_or(Rumble::OFF)
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

#[cfg(test)]
mod tests {
    use super::*;
    use coroom_input::inject::{Recorded, RecordingSink};
    use coroom_transport::crypto::Identity;
    use coroom_transport::proto::input::MouseButton;

    use crate::Role;
    use crate::latency::LatencyLedger;

    fn worker(sink: &RecordingSink) -> Worker {
        let core = Arc::new(Core::new(
            Role::Host,
            Identity::generate().expect("identidad"),
            Arc::new(LatencyLedger::default()),
            true,
        ));
        Worker::new(core, Box::new(sink.clone()))
    }

    fn key(code: u16, pressed: bool) -> InputEvent {
        InputEvent::Key { code, pressed }
    }

    /// Un paquete con `events` y el estado que resulta de aplicarlos a `before`.
    fn packet(
        worker: &mut Worker,
        guest: GuestId,
        seq: u32,
        before: &InputState,
        events: Vec<InputEvent>,
    ) -> InputState {
        texts(worker, guest, seq, before, events, 0)
    }

    /// Como [`packet`], con los textos numerados desde `text_seq`.
    fn texts(
        worker: &mut Worker,
        guest: GuestId,
        seq: u32,
        before: &InputState,
        events: Vec<InputEvent>,
        text_seq: u16,
    ) -> InputState {
        let mut state = before.clone();
        for event in &events {
            state.apply(event);
        }
        worker.handle(Job::Packet {
            guest,
            seq,
            received: Instant::now(),
            queued_us: 120,
            network_us: Some(3_000),
            events,
            text_seq,
            state: state.clone(),
            pads: Vec::new(),
        });
        state
    }

    /// Con la secuencia seguida se aplican los eventos; con un hueco, la diferencia entre la
    /// vista y el estado que llega (lo que se perdió incluido); lo antiguo se descarta y una
    /// repetición sin cambios no inyecta nada.
    #[test]
    fn lost_packets_are_reconciled_from_the_state() {
        let sink = RecordingSink::new();
        let mut worker = worker(&sink);
        let guest = GuestId(1);
        let kbm = Permissions {
            keyboard_mouse: true,
            ..Permissions::VIEW_ONLY
        };
        worker.handle(Job::Permissions {
            guest,
            permissions: kbm,
        });

        // seq 0: A abajo. Llega y se aplica tal cual.
        let s0 = packet(
            &mut worker,
            guest,
            0,
            &InputState::default(),
            vec![key(30, true)],
        );
        assert_eq!(sink.events_of(guest), vec![key(30, true)]);

        // seq 1 (se perderá): A arriba, clic izquierdo abajo y 5 px de movimiento.
        let mut s1 = s0.clone();
        for event in [
            key(30, false),
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
            InputEvent::MouseMove { dx: 5, dy: 0 },
        ] {
            s1.apply(&event);
        }
        // seq 2: B abajo y 2 px más, sobre el estado de seq 1. Llega con hueco.
        packet(
            &mut worker,
            guest,
            2,
            &s1,
            vec![key(48, true), InputEvent::MouseMove { dx: 2, dy: 0 }],
        );
        let events = sink.events_of(guest);
        assert_eq!(
            events,
            vec![
                key(30, true),
                // Reconciliación: lo que se suelta, lo que se pulsa (teclas y luego botones), el
                // movimiento que falta.
                key(30, false),
                key(48, true),
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
                InputEvent::MouseMove { dx: 7, dy: 0 },
            ]
        );

        // seq 1 llega tarde: se descarta entero.
        let late = s1.clone();
        worker.handle(Job::Packet {
            guest,
            seq: 1,
            received: Instant::now(),
            queued_us: 0,
            network_us: None,
            events: vec![key(30, false)],
            text_seq: 0,
            state: late,
            pads: Vec::new(),
        });
        assert_eq!(sink.events_of(guest).len(), 5, "lo antiguo no se aplica");

        // seq 3: repetición del estado sin eventos: nada que inyectar.
        let s2 = worker.guests[&guest].view.clone();
        packet(&mut worker, guest, 3, &s2, Vec::new());
        assert_eq!(sink.events_of(guest).len(), 5);

        let state = &worker.guests[&guest];
        assert_eq!(state.repaired, 1);
        assert_eq!(state.applied, 5);
        assert_eq!(state.dropped, 1, "el paquete tardío");
        assert_eq!(state.last_seq, Some(3));
        // Dos paquetes con eventos aplicados: dos muestras por etapa.
        assert_eq!(state.latencies.0.len(), 2);
        assert_eq!(state.queue.p50(), 120);
        assert_eq!(state.network.p50(), 3_000);
        // La tecla de seq 0 en su grupo y los cuatro eventos de la reconciliación (todos de
        // teclado y ratón) juntos en uno.
        assert_eq!(sink.batch_sizes(), vec![1, 4]);
    }

    /// Los eventos permitidos de un paquete van al sumidero por grupos de la misma clase, en
    /// orden; `ReleaseAll` cierra el grupo y va aparte; lo no permitido no rompe un grupo.
    #[test]
    fn a_packets_events_are_grouped_per_class() {
        let sink = RecordingSink::new();
        let mut worker = worker(&sink);
        let guest = GuestId(1);
        worker.handle(Job::Permissions {
            guest,
            permissions: Permissions {
                keyboard_mouse: true,
                gamepad_slot: Some(2),
                ..Permissions::VIEW_ONLY
            },
        });
        let axis = |axis, value| InputEvent::GamepadAxis {
            slot: 2,
            axis,
            value,
        };
        let events = vec![
            key(30, true),
            InputEvent::MouseMove { dx: 1, dy: 0 },
            axis(coroom_input::GamepadAxis::LeftX, 1000),
            // Otra ranura: no permitido, se descarta sin cerrar el grupo del mando.
            InputEvent::GamepadAxis {
                slot: 3,
                axis: coroom_input::GamepadAxis::LeftX,
                value: 5,
            },
            axis(coroom_input::GamepadAxis::LeftY, -1000),
            InputEvent::ReleaseAll,
            key(30, false),
        ];
        packet(&mut worker, guest, 0, &InputState::default(), events);
        // [tecla, ratón] [X, Y] ReleaseAll [tecla].
        assert_eq!(sink.batch_sizes(), vec![2, 2, 1]);
        let log = sink.log();
        assert_eq!(log[0], Recorded::GamepadAssigned { slot: 2 });
        assert_eq!(log[5], Recorded::ReleaseAll { guest });
        assert_eq!(
            log[6],
            Recorded::Event {
                guest,
                event: key(30, false)
            }
        );
        let state = &worker.guests[&guest];
        assert_eq!(state.applied, 6);
        assert_eq!(state.dropped, 1);
    }

    /// El texto sin tecla física es entrada de teclado: sin el permiso de teclado y ratón se
    /// descarta como una tecla, y con él llega al sumidero en el grupo del teclado.
    #[test]
    fn text_needs_the_keyboard_permission_like_a_key() {
        let sink = RecordingSink::new();
        let mut worker = worker(&sink);
        let guest = GuestId(1);
        let text = |ch| InputEvent::Text { ch };
        worker.handle(Job::Permissions {
            guest,
            permissions: Permissions {
                gamepad_slot: Some(1),
                ..Permissions::VIEW_ONLY
            },
        });
        let s0 = texts(
            &mut worker,
            guest,
            0,
            &InputState::default(),
            vec![text('ñ'), key(30, true)],
            0,
        );
        assert!(
            sink.events_of(guest).is_empty(),
            "sin permiso no llega nada"
        );
        assert_eq!(worker.guests[&guest].dropped, 2);

        worker.handle(Job::Permissions {
            guest,
            permissions: Permissions {
                keyboard_mouse: true,
                ..Permissions::VIEW_ONLY
            },
        });
        texts(
            &mut worker,
            guest,
            1,
            &s0,
            vec![key(30, true), text('a')],
            1,
        );
        assert_eq!(sink.events_of(guest), vec![key(30, true), text('a')]);
        assert_eq!(
            sink.batch_sizes().last(),
            Some(&2),
            "en el grupo del teclado"
        );
    }

    /// Un texto se teclea una sola vez y en orden aunque se pierda su paquete o llegue repetido:
    /// el invitado lo repite en los paquetes siguientes (con su número) y el anfitrión salta los
    /// que ya tecleó, también tras un hueco reconciliado con el estado.
    #[test]
    fn texts_are_typed_once_and_in_order_despite_losses_and_repeats() {
        let sink = RecordingSink::new();
        let mut worker = worker(&sink);
        let guest = GuestId(1);
        let text = |ch| InputEvent::Text { ch };
        worker.handle(Job::Permissions {
            guest,
            permissions: Permissions {
                keyboard_mouse: true,
                ..Permissions::VIEW_ONLY
            },
        });
        // seq 0: «h» (texto 0).
        let s0 = texts(
            &mut worker,
            guest,
            0,
            &InputState::default(),
            vec![text('h')],
            0,
        );
        // seq 1 se pierde: «o» (texto 1), con la «h» repetida delante, y la A pulsada.
        let mut s1 = s0.clone();
        s1.apply(&key(30, true));
        // seq 2 llega con hueco: repite «h» y «o» y trae «l» (texto 2).
        let s2 = texts(
            &mut worker,
            guest,
            2,
            &s1,
            vec![text('h'), text('o'), text('l')],
            0,
        );
        // seq 3, la repetición de seq 2 (solo el estado y los textos que siguen en la ventana).
        let s3 = texts(&mut worker, guest, 3, &s2, vec![text('o'), text('l')], 1);
        // seq 4: «a» nueva detrás de la «l» repetida.
        texts(&mut worker, guest, 4, &s3, vec![text('l'), text('a')], 2);
        assert_eq!(
            sink.events_of(guest),
            vec![
                text('h'),
                // La reconciliación del hueco (la A) y, detrás, los textos que faltaban.
                key(30, true),
                text('o'),
                text('l'),
                text('a'),
            ]
        );
        assert_eq!(worker.guests[&guest].texts.next, Some(4));
    }

    #[test]
    fn text_order_counts_repeats_and_gives_up_on_texts_no_longer_repeated() {
        let mut order = TextOrder::default();
        assert_eq!(order.accept(10, 0), (0, 0), "sin textos no cambia nada");
        assert_eq!(order.next, None);
        assert_eq!(order.accept(10, 2), (0, 0));
        assert_eq!(order.accept(10, 3), (2, 0), "10 y 11 ya, 12 nuevo");
        assert_eq!(order.accept(11, 2), (2, 0), "todo repetido");
        assert_eq!(order.next, Some(13));
        // El 13 y el 14 dejaron de repetirse sin llegar: se pierden y se sigue por el 15.
        assert_eq!(order.accept(15, 1), (0, 2));
        assert_eq!(order.next, Some(16));
        // Con vuelta en `u16`.
        let mut order = TextOrder {
            next: Some(u16::MAX),
        };
        assert_eq!(order.accept(u16::MAX - 1, 3), (1, 0));
        assert_eq!(order.next, Some(1));
    }

    /// Sin nada compartido («Dejar de compartir» o una sala solo de voz) no se inyecta nada de
    /// ningún invitado (en Windows `SendInput` y los mandos no dependen de la captura): lo pulsado
    /// se suelta al dejar de compartir y, al volver, se sigue sin reconstruir lo de entretanto.
    #[test]
    fn nothing_is_injected_while_nothing_is_shared() {
        let sink = RecordingSink::new();
        let mut worker = worker(&sink);
        let guest = GuestId(1);
        worker.handle(Job::Permissions {
            guest,
            permissions: Permissions {
                keyboard_mouse: true,
                ..Permissions::VIEW_ONLY
            },
        });
        let s0 = packet(
            &mut worker,
            guest,
            0,
            &InputState::default(),
            vec![key(30, true)],
        );
        assert_eq!(sink.events_of(guest), vec![key(30, true)]);
        // Deja de compartir con la A pulsada: se suelta.
        worker.core.shared.set_sharing(false);
        worker.housekeeping(Instant::now());
        assert!(worker.paused);
        assert_eq!(sink.log().last(), Some(&Recorded::ReleaseAll { guest }));
        // Lo que llega ahora no se inyecta (y la pausa del anfitrión no lo reanuda).
        let s1 = packet(&mut worker, guest, 1, &s0, vec![key(48, true)]);
        worker.handle(Job::Paused(false));
        let s2 = packet(&mut worker, guest, 2, &s1, vec![key(48, false)]);
        assert_eq!(sink.events_of(guest), vec![key(30, true)]);
        assert_eq!(worker.guests[&guest].dropped, 2);
        // Vuelve a compartir: el siguiente paquete se aplica tal cual.
        worker.core.shared.set_sharing(true);
        packet(&mut worker, guest, 3, &s2, vec![key(57, true)]);
        assert!(!worker.paused);
        assert_eq!(sink.events_of(guest), vec![key(30, true), key(57, true)]);
        // Con la pausa del anfitrión puesta, volver a compartir no la quita.
        worker.handle(Job::Paused(true));
        worker.core.shared.set_sharing(false);
        worker.core.shared.set_sharing(true);
        worker.housekeeping(Instant::now());
        assert!(worker.paused);
    }

    /// Un invitado del que no llega nada en [`STALE_AFTER`] con algo pulsado: se suelta lo suyo y
    /// su siguiente paquete, aunque siga la secuencia, se reconcilia con la vista (vuelve a
    /// pulsar lo que de verdad mantiene).
    #[test]
    fn a_silent_guest_is_released_and_then_reconciled() {
        let sink = RecordingSink::new();
        let mut worker = worker(&sink);
        let guest = GuestId(1);
        worker.handle(Job::Permissions {
            guest,
            permissions: Permissions {
                keyboard_mouse: true,
                ..Permissions::VIEW_ONLY
            },
        });
        let s0 = packet(
            &mut worker,
            guest,
            0,
            &InputState::default(),
            vec![key(30, true)],
        );
        let received = Instant::now();
        // Todavía no: ni a mitad de plazo ni sin nada pulsado.
        worker.expire_stale(received + STALE_AFTER / 2);
        assert_eq!(sink.len(), 1);
        worker.expire_stale(received + STALE_AFTER + Duration::from_millis(1));
        assert_eq!(sink.log().last(), Some(&Recorded::ReleaseAll { guest }));
        let state = &worker.guests[&guest];
        assert!(state.stale && !state.view.any_pressed());
        // Una sola vez.
        worker.expire_stale(received + STALE_AFTER * 2);
        assert_eq!(sink.len(), 2);

        // Llega el keepalive (seq 1, sin eventos, la tecla sigue pulsada): se reconcilia.
        packet(&mut worker, guest, 1, &s0, Vec::new());
        assert_eq!(sink.events_of(guest), vec![key(30, true), key(30, true)]);
        let state = &worker.guests[&guest];
        assert!(!state.stale && state.view.key_pressed(30));
        assert_eq!(state.repaired, 1);

        // Sin nada pulsado, el silencio no hace nada.
        let view = state.view.clone();
        let s2 = packet(&mut worker, guest, 2, &view, vec![key(30, false)]);
        assert!(!s2.any_pressed());
        worker.expire_stale(Instant::now() + STALE_AFTER * 3);
        assert_eq!(sink.len(), 4);
        assert!(!worker.guests[&guest].stale);
    }

    /// Las estadísticas se publican con cadencia y lo inmediato (permisos, mando) al momento.
    #[test]
    fn stats_are_published_on_a_cadence() {
        let sink = RecordingSink::new();
        let mut worker = worker(&sink);
        let guest = GuestId(1);
        worker.handle(Job::Permissions {
            guest,
            permissions: Permissions {
                keyboard_mouse: true,
                ..Permissions::VIEW_ONLY
            },
        });
        assert!(worker.guests[&guest].dirty);
        worker.publish(false);
        assert!(
            !worker.guests[&guest].dirty,
            "lo inmediato sale sin esperar"
        );
        packet(
            &mut worker,
            guest,
            0,
            &InputState::default(),
            vec![key(30, true)],
        );
        assert!(worker.guests[&guest].stats_changed);
        worker.publish(false);
        assert!(
            worker.guests[&guest].stats_changed,
            "las muestras esperan a la publicación periódica"
        );
        worker.publish(true);
        assert!(!worker.guests[&guest].stats_changed);
    }
}
