//! Anfitrión de la sesión remota: sala, grupo MLS, conexiones con los invitados, negociación del
//! códec, envío cifrado, cursor y audio del juego, voz propia y reenvío de la voz de los
//! invitados.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use bytes::Bytes;
use coroom_audio::processing::VoiceProcessor;
use coroom_capture::{CaptureError, CapturedFrame, Capturer, CursorState};
use coroom_codec::{
    Backend, Codec, DEFAULT_SLICES, EncoderConfig, EncoderSet, VideoFormat, encoder_label,
};
use coroom_input::InputSink;
use coroom_transport::connect::{self, Connector, ExtraCandidates, RelayInfo};
use coroom_transport::crypto::hpke::{self, PairwiseOffer};
use coroom_transport::crypto::sframe::MediaKey;
use coroom_transport::crypto::{RoomGroup, SafetyNumber, sha256_fingerprint};
use coroom_transport::media::{
    DEFAULT_SOURCE, MediaSender, MediaSenderConfig, PROBE_SPACING, PROBE_TRAINS, audio_datagram,
    probe_pace_kbps,
};
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::control::{
    ChatEntry, ControlMessage, Hello, MEMBER_NAME_MAX, MEMBERS_MAX, Member as WireMember,
    Role as WireRole, SealedChat,
};
use coroom_transport::proto::media::{ObjectHeader, Track};
use coroom_transport::proto::signal::{ErrorCode, SignalMessage};
use coroom_transport::quic::{ControlReceiver, ControlSender, ControlStream};
use coroom_transport::rate::{Congestion, RateConfig};
use coroom_transport::signal::{ServerUrl, SignalClient, SignalSender};
use coroom_transport::{Endpoint, EndpointOptions, Path, PeerConnection, PublicKey};
use crossbeam_channel::{Receiver, bounded};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tracing::debug;

use super::audio::{
    AudioOut, AudioReader, HostAudio, Packet, Player, TalkMode, extend_us, game_capture_loop,
    packet_channel, play_loop, voice_capture_loop,
};
use super::chat::{ChatAuthor, ChatLine};
use super::input::{HostInput, Job};
use super::negotiate::{
    GuestCapabilities, choose_codec, choose_format, codec_to_wire, format_shortfall,
    format_to_wire, shape_to_message,
};
use super::wire::{Payload, pairwise_info, require_pq_offer};
use super::{
    CAPABILITIES_TIMEOUT, CURSOR_PERIOD, CURSOR_STREAM_PERIOD, Command, Core, GuestStatus,
    RATE_PERIOD, REPLY_TIMEOUT, RemoteConfig, SessionEvent, Started, VideoChange,
    advertised_candidates, lock,
};
use crate::latency::{AudioTrack, Stage};
use crate::loopback::{
    CHANNEL_CAPACITY, Compressed, EncodeControl, LoopbackConfig, PipelineError, capture_frames_for,
    capture_loop, encode_loop, recv_or_stop, spawn_stage,
};
use crate::stats::{StatsWindow, bump};
use crate::{GuestId, Permissions, SessionError, SessionState};

/// Clave de época pendiente de instalar (la del último invitado añadido).
type PendingEpoch = (u64, Vec<(u32, MediaKey)>);

/// Una petición de intra para un grupo anterior al último intra enviado se da por servida si ese
/// intra salió hace menos de esto (el invitado lo pidió antes de que le llegase).
const STALE_REQUEST_WINDOW: Duration = Duration::from_secs(1);

/// Margen de la espera entre dos intra forzados, además del RTT y dos intervalos.
const KEYFRAME_MARGIN: Duration = Duration::from_millis(10);

/// Espera máxima entre dos intra forzados con el retroceso ([`KeyframeGate`]).
const MAX_KEYFRAME_SPACING: Duration = Duration::from_secs(1);

/// Intra forzados seguidos (cada uno a menos de [`MAX_KEYFRAME_SPACING`] del anterior) a partir de
/// los que la espera ya no crece (×2 con cada uno: 1, 2, 4, 8 y 16 veces la base).
const MAX_KEYFRAME_BACKOFF: u32 = 4;

/// Qué hacer con un `KeyframeRequest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyframeDecision {
    /// Forzar un intra ya.
    Now,
    /// Forzarlo pasado este tiempo (ya salió uno hace poco): las peticiones de entretanto se
    /// agrupan en este.
    Later(Duration),
    /// Ya está servida (viene un intra de camino o hay uno aplazado).
    Ignore,
}

/// Limita los intra forzados por `KeyframeRequest` y agrupa los de todos los invitados. Sin esto,
/// cada grupo perdido pedía un intra y cada intra (el fotograma más grande) desbordaba otra vez el
/// búfer del receptor: una tormenta de intra que congelaba el vídeo decenas de segundos.
///
/// - Una petición de un grupo anterior al último intra enviado, si este salió hace menos de
///   [`STALE_REQUEST_WINDOW`], ya está servida.
/// - Si hay uno pedido que aún no ha salido, o uno aplazado, se agrupa con él.
/// - Entre dos intra forzados pasa al menos `RTT + 2 intervalos + margen` desde que terminó de
///   salir el anterior (lo que tarda en llegar y en saberse si llegó), duplicado con cada intra
///   forzado seguido hasta [`MAX_KEYFRAME_SPACING`]; las peticiones de antes se aplazan a ese
///   momento.
#[derive(Debug, Default)]
pub(crate) struct KeyframeGate {
    state: std::sync::Mutex<GateState>,
}

#[derive(Debug, Default)]
struct GateState {
    /// Último intra enviado: su grupo y cuándo terminó de salir.
    sent: Option<(u32, Instant)>,
    /// Último intra forzado por una petición.
    forced: Option<Instant>,
    /// Intra forzados seguidos.
    backoff: u32,
    /// Hay uno aplazado.
    deferred: bool,
}

impl KeyframeGate {
    /// El intra del grupo `group` terminó de salir en `at`.
    pub(crate) fn on_sent(&self, group: u32, at: Instant) {
        lock(&self.state).sent = Some((group, at));
    }

    /// Decide qué hacer con una petición del grupo `group` con este RTT e intervalo.
    fn on_request(
        &self,
        group: u32,
        rtt: Duration,
        interval: Duration,
        now: Instant,
    ) -> KeyframeDecision {
        let mut state = lock(&self.state);
        if let Some((sent_group, at)) = state.sent
            && (sent_group.wrapping_sub(group) as i32) > 0
            && now.saturating_duration_since(at) < STALE_REQUEST_WINDOW
        {
            return KeyframeDecision::Ignore;
        }
        // Pedido y aún sin salir (si en un segundo no salió, el codificador no estaba: se vuelve a
        // pedir).
        let pending = state.forced.is_some_and(|forced| {
            state.sent.is_none_or(|(_, at)| at < forced)
                && now.saturating_duration_since(forced) < MAX_KEYFRAME_SPACING
        });
        if pending || state.deferred {
            return KeyframeDecision::Ignore;
        }
        let base = rtt + interval * 2 + KEYFRAME_MARGIN;
        let spacing = (base * (1 << state.backoff)).min(MAX_KEYFRAME_SPACING);
        let since = state.sent.map(|(_, at)| at);
        match since {
            Some(at) if now.saturating_duration_since(at) < spacing => {
                state.deferred = true;
                KeyframeDecision::Later(spacing - now.saturating_duration_since(at))
            }
            _ => {
                Self::force(&mut state, now);
                KeyframeDecision::Now
            }
        }
    }

    /// Lanza el intra aplazado.
    fn fire_deferred(&self, now: Instant) {
        let mut state = lock(&self.state);
        state.deferred = false;
        Self::force(&mut state, now);
    }

    fn force(state: &mut GateState, now: Instant) {
        let quick = state
            .forced
            .is_some_and(|at| now.saturating_duration_since(at) < MAX_KEYFRAME_SPACING);
        state.backoff = if quick {
            (state.backoff + 1).min(MAX_KEYFRAME_BACKOFF)
        } else {
            0
        };
        state.forced = Some(now);
    }
}

/// Emisor de una conexión directa con un invitado.
struct DirectLink {
    sender: Arc<MediaSender>,
    control: ControlSender,
}

/// Emisor único hacia el relay y el control de cada invitado que llega por él.
struct RelayLink {
    sender: Arc<MediaSender>,
    guests: BTreeMap<GuestId, ControlSender>,
}

/// Un destino del fotograma cifrado: un emisor y los controles de los invitados a los que llega.
struct Target {
    sender: Arc<MediaSender>,
    controls: Vec<ControlSender>,
}

/// Conexiones de medios del anfitrión.
#[derive(Default)]
pub(crate) struct Links {
    direct: BTreeMap<GuestId, DirectLink>,
    relay: Option<RelayLink>,
}

impl Links {
    fn targets(&self) -> Vec<Target> {
        let mut out: Vec<Target> = self
            .direct
            .values()
            .map(|link| Target {
                sender: Arc::clone(&link.sender),
                controls: vec![link.control.clone()],
            })
            .collect();
        if let Some(relay) = &self.relay
            && !relay.guests.is_empty()
        {
            out.push(Target {
                sender: Arc::clone(&relay.sender),
                controls: relay.guests.values().cloned().collect(),
            });
        }
        out
    }

    fn connected(&self) -> usize {
        self.direct.len() + self.relay.as_ref().map_or(0, |relay| relay.guests.len())
    }

    fn contains(&self, guest: GuestId) -> bool {
        self.direct.contains_key(&guest)
            || self
                .relay
                .as_ref()
                .is_some_and(|relay| relay.guests.contains_key(&guest))
    }

    fn controls(&self) -> Vec<ControlSender> {
        self.targets()
            .into_iter()
            .flat_map(|target| target.controls)
            .collect()
    }

    /// Control de cada invitado conectado.
    fn guest_controls(&self) -> Vec<(GuestId, ControlSender)> {
        let mut out: Vec<(GuestId, ControlSender)> = self
            .direct
            .iter()
            .map(|(guest, link)| (*guest, link.control.clone()))
            .collect();
        if let Some(relay) = &self.relay {
            out.extend(
                relay
                    .guests
                    .iter()
                    .map(|(guest, control)| (*guest, control.clone())),
            );
        }
        out
    }

    fn senders(&self) -> Vec<Arc<MediaSender>> {
        let mut out: Vec<Arc<MediaSender>> = self
            .direct
            .values()
            .map(|link| Arc::clone(&link.sender))
            .collect();
        if let Some(relay) = &self.relay {
            out.push(Arc::clone(&relay.sender));
        }
        out
    }

    /// Conexiones con invitados conectados (la del relay una vez, si tiene alguno): por ellas van
    /// los datagramas de control del anfitrión (el cursor numerado, que el relay abre en abanico).
    fn connections(&self) -> Vec<PeerConnection> {
        let mut out: Vec<PeerConnection> = self
            .direct
            .values()
            .map(|link| link.sender.connection().clone())
            .collect();
        if let Some(relay) = &self.relay
            && !relay.guests.is_empty()
        {
            out.push(relay.sender.connection().clone());
        }
        out
    }

    /// Bitrate para el codificador: el mínimo de los objetivos de todos los emisores con
    /// invitados, descontada la FEC de cada uno ([`MediaSender::encoder_target_kbps`]).
    fn min_target(&self) -> Option<u32> {
        self.targets()
            .iter()
            .map(|target| target.sender.encoder_target_kbps())
            .min()
    }

    /// Ruta, RTT y objetivo de cada invitado conectado.
    fn status(&self) -> Vec<LinkStatus> {
        let mut out: Vec<_> = self
            .direct
            .iter()
            .map(|(guest, link)| LinkStatus {
                guest: *guest,
                path: Path::Direct,
                remote: link.sender.connection().remote_address(),
                rtt_ms: link.sender.connection().rtt().as_secs_f64() * 1000.0,
                target_kbps: Some(link.sender.target_kbps()),
            })
            .collect();
        if let Some(relay) = &self.relay {
            let conn = relay.sender.connection();
            let rtt_ms = conn.rtt().as_secs_f64() * 1000.0;
            for guest in relay.guests.keys() {
                out.push(LinkStatus {
                    guest: *guest,
                    path: Path::Relayed,
                    remote: conn.remote_address(),
                    rtt_ms,
                    target_kbps: relay.sender.source_target_kbps(u64::from(guest.0)),
                });
            }
        }
        out
    }
}

/// Estado del enlace de un invitado para su [`GuestStatus`].
struct LinkStatus {
    guest: GuestId,
    path: Path,
    remote: std::net::SocketAddr,
    rtt_ms: f64,
    target_kbps: Option<u32>,
}

/// Arranca los hilos del anfitrión y su tarea de red. El codificador no se crea aquí: espera a
/// conocer lo que decodifica el primer invitado (ver [`renegotiate`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn start(
    core: &Arc<Core>,
    config: RemoteConfig,
    mut capturer: Box<dyn Capturer>,
    encoders: EncoderSet,
    video: LoopbackConfig,
    sink: Box<dyn InputSink>,
    host_audio: Option<HostAudio>,
    handle: &Handle,
    commands: mpsc::UnboundedReceiver<Command>,
) -> Result<Started, SessionError> {
    let media = |err: &dyn std::fmt::Display| SessionError::Media(err.to_string());
    // Los códecs necesitan un tamaño par: uno impar se recorta una columna o una fila, como cada
    // fotograma de la captura (una ventana de 1199×808 sale a 1198×808), en vez de no abrir.
    let video = LoopbackConfig {
        width: video.width & !1,
        height: video.height & !1,
        ..video
    };
    if video.width == 0 || video.height == 0 || video.fps == 0 || video.bitrate_kbps == 0 {
        return Err(media(&PipelineError::Codec(
            coroom_codec::CodecError::Unsupported(format!(
                "el vídeo necesita al menos 2×2 píxeles, fps y bitrate, no {}×{} a {} fps y \
                 {} kbit/s",
                video.width, video.height, video.fps, video.bitrate_kbps
            )),
        )));
    }
    if encoders.is_empty() {
        return Err(media(&PipelineError::Codec(
            coroom_codec::CodecError::Unsupported(
                "no hay ningún codificador disponible con las opciones dadas".into(),
            ),
        )));
    }
    let target = capturer
        .targets()
        .map_err(|err| media(&PipelineError::Capture(err)))?
        .into_iter()
        .next()
        .ok_or_else(|| {
            media(&PipelineError::Capture(CaptureError::Unsupported(
                "no hay nada que capturar".into(),
            )))
        })?;
    // Sin copias: la captura entrega lo que importa el codificador preferido; si se
    // negocia otro que no lo importa, ese baja la imagen a CPU (con las copias contadas).
    // Con 4:4:4 o HDR pedidos la captura va en CPU: esas conversiones son de CPU.
    let gpu = capturer.set_gpu_frames(video.gpu_frames(&encoders.gpu_input()));
    capturer.set_options(video.capture_options());
    *lock(&core.capture_setup) = Some((gpu, video.capture_options()));
    capturer
        .start(&target)
        .map_err(|err| media(&PipelineError::Capture(err)))?;

    let links = Arc::new(std::sync::Mutex::new(Links::default()));
    let encode = Arc::new(EncodeControl::new(super::start_kbps(video.bitrate_kbps)));
    let gate = Arc::new(KeyframeGate::default());
    let (input, inject_thread) = HostInput::start(core, sink)?;
    *lock(&core.host_input) = Some(Arc::clone(&input));
    let (raw_tx, raw_rx) = bounded::<CapturedFrame>(CHANNEL_CAPACITY);
    let (enc_tx, enc_rx) = bounded::<Compressed>(CHANNEL_CAPACITY);
    let shared = Arc::clone(&core.shared);
    let mut threads = Vec::with_capacity(4);
    threads.push(("inject", inject_thread));

    let spawn_error = |err: PipelineError| SessionError::Media(err.to_string());
    {
        let (shared, ledger, evict) = (
            Arc::clone(&shared),
            Arc::clone(&core.ledger),
            raw_rx.clone(),
        );
        let handle = spawn_stage(&core.shared, "capture", move || {
            capture_loop(capturer, raw_tx, evict, &shared, &ledger)
        })
        .map_err(spawn_error)?;
        threads.push(("capture", handle));
    }
    {
        let (shared, ledger, encode) = (
            Arc::clone(&shared),
            Arc::clone(&core.ledger),
            Arc::clone(&encode),
        );
        let handle = spawn_stage(&core.shared, "encode", move || {
            encode_loop(None, raw_rx, enc_tx, &shared, &ledger, Some(&encode))
        })
        .map_err(spawn_error)?;
        threads.push(("encode", handle));
    }
    {
        let (core, links, handle, gate) = (
            Arc::clone(core),
            Arc::clone(&links),
            handle.clone(),
            Arc::clone(&gate),
        );
        let frames = video.frames;
        let thread = spawn_stage(&core.shared.clone(), "send", move || {
            send_loop(&core, &enc_rx, &links, &handle, frames, &gate)
        })
        .map_err(spawn_error)?;
        threads.push(("send", thread));
    }

    let audio_info = match host_audio {
        Some(host_audio) => start_audio(core, &links, host_audio, &mut threads)?,
        None => AudioInfo::default(),
    };

    let main = handle.spawn(host_main(
        Arc::clone(core),
        config,
        video,
        encoders,
        links,
        encode,
        gate,
        input,
        audio_info,
        commands,
    ));
    Ok(Started { main, threads })
}

/// Pistas de audio que anuncia el anfitrión: `(bitrate, ms por paquete)`.
#[derive(Debug, Clone, Copy, Default)]
struct AudioInfo {
    game: Option<(u32, u8)>,
    voice: Option<(u32, u8)>,
}

/// Arranca los hilos de audio del anfitrión (`audio-play`, `audio-cap`, `voice-cap`).
fn start_audio(
    core: &Arc<Core>,
    links: &Arc<std::sync::Mutex<Links>>,
    host_audio: HostAudio,
    threads: &mut Vec<(&'static str, std::thread::JoinHandle<()>)>,
) -> Result<AudioInfo, SessionError> {
    let spawn_error = |err: PipelineError| SessionError::Media(err.to_string());
    let control = core.audio.clone();
    control.set_voice_allowed(true);
    let frame_ms = if host_audio.frame_ms >= 20 { 20 } else { 10 };
    let out = Arc::new(HostAudioOut {
        core: Arc::clone(core),
        links: Arc::clone(links),
    });
    let processor =
        host_audio
            .voice
            .as_ref()
            .and_then(|voice| match VoiceProcessor::new(voice.processing) {
                Ok(processor) => Some(processor),
                Err(err) => {
                    core.warn(format!("sin procesado de voz: {err}"));
                    None
                }
            });
    let render = processor
        .as_ref()
        .filter(|processor| processor.echo_cancellation())
        .map(VoiceProcessor::render_feed);
    let mut info = AudioInfo::default();
    if let Some(speaker) = host_audio.speaker {
        let (tx, rx) = packet_channel();
        *lock(&core.audio_tx) = Some(tx);
        let (thread_core, control) = (Arc::clone(core), control.clone());
        let handle = spawn_stage(&core.shared, "audio-play", move || {
            let clock = Arc::clone(&thread_core);
            let to_local = move |capture_us: u32| {
                let full = extend_us(clock.clock_us(Instant::now()), capture_us);
                clock.clock_origin.checked_add(Duration::from_micros(full))
            };
            let player = Player {
                sink: speaker,
                control,
                render,
                av_sync: false,
            };
            play_loop(&thread_core, player, rx, to_local);
            Ok(())
        })
        .map_err(spawn_error)?;
        threads.push(("audio-play", handle));
    }
    if let Some(game) = host_audio.game {
        let bitrate = if host_audio.game_bitrate == 0 {
            super::audio::DEFAULT_GAME_BITRATE
        } else {
            host_audio.game_bitrate
        };
        info.game = Some((bitrate, frame_ms));
        core.game_audio.store(true, Ordering::Release);
        let (thread_core, out) = (Arc::clone(core), Arc::clone(&out));
        let handle = spawn_stage(&core.shared, "audio-cap", move || {
            game_capture_loop(&thread_core, game, bitrate, frame_ms, out.as_ref());
            Ok(())
        })
        .map_err(spawn_error)?;
        threads.push(("audio-cap", handle));
    }
    if let (Some(voice), Some(processor)) = (host_audio.voice, processor) {
        control.set_voice_enabled(true);
        control.set_push_to_talk(voice.talk == TalkMode::PushToTalk);
        info.voice = Some((voice.bitrate, frame_ms));
        let (thread_core, out) = (Arc::clone(core), Arc::clone(&out));
        let handle = spawn_stage(&core.shared, "voice-cap", move || {
            voice_capture_loop(
                &thread_core,
                voice,
                processor,
                frame_ms,
                &control,
                out.as_ref(),
            );
            Ok(())
        })
        .map_err(spawn_error)?;
        threads.push(("voice-cap", handle));
    }
    Ok(info)
}

/// Destinos del audio del anfitrión: los enlaces directos y el relay.
struct HostAudioOut {
    core: Arc<Core>,
    links: Arc<std::sync::Mutex<Links>>,
}

/// Rellena `out` con los invitados con permiso de voz.
fn voice_permitted(core: &Core, out: &mut Vec<GuestId>) {
    out.clear();
    out.extend(
        lock(&core.guests)
            .values()
            .filter(|status| status.permissions.voice)
            .map(|status| status.guest),
    );
}

/// Envía `datagram` a los invitados que cumplen `allowed` (menos `exclude`): por su enlace directo
/// o, si alguno llega por el relay, una vez al relay (que abre en abanico). Devuelve si salió.
fn send_to(
    links: &Links,
    datagram: &Bytes,
    allowed: impl Fn(&GuestId) -> bool,
    exclude: Option<GuestId>,
) -> bool {
    let mut sent = false;
    for (guest, link) in &links.direct {
        if Some(*guest) != exclude
            && allowed(guest)
            && link
                .sender
                .connection()
                .send_datagram(datagram.clone())
                .is_ok()
        {
            sent = true;
        }
    }
    if let Some(relay) = &links.relay
        && relay
            .guests
            .keys()
            .any(|guest| Some(*guest) != exclude && allowed(guest))
        && relay
            .sender
            .connection()
            .send_datagram(datagram.clone())
            .is_ok()
    {
        sent = true;
    }
    sent
}

impl AudioOut for HostAudioOut {
    fn send(&self, track: Track, seq: u32, sealed: &[u8]) -> bool {
        let epoch = self.core.epoch.load(Ordering::Acquire) as u16;
        let Some(datagram) = audio_datagram(track, seq, epoch, sealed) else {
            return false;
        };
        // La voz de la sala la oyen todos los invitados; el permiso de voz es para hablar.
        let links = lock(&self.links);
        send_to(&links, &datagram, |_| true, None)
    }

    fn has_targets(&self, _track: Track) -> bool {
        lock(&self.links).connected() > 0
    }

    fn host_us(&self, at: Instant) -> Option<u64> {
        Some(self.core.clock_us(at))
    }

    fn speaking(&self, speaking: bool) {
        self.core.audio.set_talker(0, speaking);
        let message = ControlMessage::VoiceState { guest: 0, speaking };
        for (_, control) in lock(&self.links).guest_controls() {
            let _ = control.send(&message);
        }
    }
}

/// Lee la voz de los invitados que llega por `conn` (la directa de `only` o la del relay): la
/// descifra para saber quién habla y oírla, comprueba su permiso y la reenvía tal cual a los
/// demás invitados.
fn voice_reader(ctx: &Arc<HostCtx>, conn: PeerConnection, only: Option<GuestId>) {
    let core = Arc::clone(&ctx.core);
    let thread_ctx = Arc::clone(ctx);
    let mut decryptor = core.ring.decryptor();
    let mut plain = Vec::with_capacity(2048);
    let mut permitted = Vec::with_capacity(16);
    let reader = AudioReader::spawn(&core, conn, move |datagram| {
        let (ctx, core) = (&thread_ctx, &thread_ctx.core);
        let arrived = Instant::now();
        let Ok((header, payload)) = ObjectHeader::parse_datagram(&datagram) else {
            return;
        };
        if header.track != Track::Voice {
            return;
        }
        let guest = if core.sframe {
            match decryptor.decrypt_into(payload, &mut plain) {
                Ok((info, _)) if info.track == Track::Voice as u8 => {
                    let leaf = info.sender_leaf;
                    lock(&core.guests)
                        .values()
                        .find(|status| status.leaf == leaf)
                        .map(|status| status.guest)
                }
                _ => None,
            }
        } else {
            plain.clear();
            plain.extend_from_slice(payload);
            only
        };
        let Some(guest) = guest.filter(|guest| only.is_none_or(|only| only == *guest)) else {
            return;
        };
        voice_permitted(core, &mut permitted);
        if !permitted.contains(&guest) {
            // Sin permiso de voz: ni se oye ni se reenvía.
            return;
        }
        {
            // A todos los demás: oír a la sala no necesita permiso.
            let links = lock(&ctx.links);
            send_to(&links, &datagram, |_| true, Some(guest));
        }
        if let Some(tx) = lock(&core.audio_tx).as_ref()
            && let Some(packet) = Packet::parse(AudioTrack::Voice, guest.0, &plain, arrived)
        {
            let _ = tx.try_send(packet);
        }
    });
    // Termina sola al cerrarse la conexión o la sesión.
    if let Err(err) = reader {
        core.warn(err);
    }
}

/// Espera a que terminen todos los futuros (sin `futures`: un `poll_fn` que los sondea a todos).
async fn join_all<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut futures: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (future, output) in futures.iter_mut().zip(outputs.iter_mut()) {
            if output.is_none() {
                match future.as_mut().poll(cx) {
                    Poll::Ready(value) => *output = Some(value),
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    outputs.into_iter().flatten().collect()
}

/// Hilo `send`: cifra cada fotograma una vez y lo entrega a todos los emisores; después manda a
/// cada invitado las marcas de tiempo del anfitrión.
fn send_loop(
    core: &Core,
    rx: &Receiver<Compressed>,
    links: &std::sync::Mutex<Links>,
    handle: &Handle,
    frames: Option<u64>,
    gate: &KeyframeGate,
) -> Result<(), PipelineError> {
    let shared = &core.shared;
    let ledger = &core.ledger;
    let mut encryptor = core.ring.encryptor(core.own_leaf.load(Ordering::Acquire));
    let mut sealed: Vec<u8> = Vec::new();
    // Los grupos son consecutivos por enlace: el receptor toma un hueco por un grupo perdido.
    let mut group: u32 = 0;
    let mut sent = 0u64;
    while let Some(frame) = recv_or_stop(rx, shared) {
        let targets = lock(links).targets();
        if targets.is_empty() {
            continue;
        }
        let payload: &[u8] = if core.sframe {
            match encryptor.encrypt_into(Track::Video as u8, &frame.data, &mut sealed) {
                Ok(_) => &sealed,
                Err(err) => {
                    debug!(error = %err, "no se pudo cifrar el fotograma");
                    ledger.record_drop(Stage::Encrypted);
                    shared.request_keyframe();
                    continue;
                }
            }
        } else {
            &frame.data
        };
        ledger.mark(frame.frame_id, Stage::Encrypted, Instant::now());
        // Un enlace atascado (su cola de QUIC lleva más de STALE_QUEUE) no recibe este fotograma:
        // saldría tarde y retrasaría el intra que lo arregla. Mientras dure se piden intra, así el
        // primero que salga cuando se vacíe la cola ya es uno (los grupos no enviados los da por
        // perdidos el receptor sin esperar al intra).
        let (targets, stalled): (Vec<_>, Vec<_>) = targets
            .into_iter()
            .partition(|target| !target.sender.stalled());
        if !stalled.is_empty() {
            bump(&shared.counters.stalled_sends);
            shared.request_keyframe();
            if targets.is_empty() {
                ledger.record_drop(Stage::Sent);
                group = group.wrapping_add(1);
                continue;
            }
        }
        let results = if targets.len() == 1 {
            vec![handle.block_on(targets[0].sender.send_frame(
                Track::Video,
                group,
                frame.keyframe,
                payload,
            ))]
        } else {
            handle.block_on(join_all(
                targets
                    .iter()
                    .map(|target| {
                        target
                            .sender
                            .send_frame(Track::Video, group, frame.keyframe, payload)
                    })
                    .collect(),
            ))
        };
        let sent_at = Instant::now();
        ledger.mark(frame.frame_id, Stage::Sent, sent_at);
        bump(&shared.counters.sent);
        if frame.keyframe {
            gate.on_sent(group, sent_at);
        }
        for result in &results {
            if let Err(err) = result {
                debug!(error = %err, group, "no se pudo enviar el fotograma a un invitado");
            }
        }
        if let Some(timeline) = ledger.timeline(frame.frame_id)
            && let (Some(captured), Some(encoded), Some(encrypted)) = (
                timeline.at(Stage::Captured),
                timeline.at(Stage::Encoded),
                timeline.at(Stage::Encrypted),
            )
        {
            for target in &targets {
                let sender = &target.sender;
                let timing = ControlMessage::FrameTiming {
                    track: Track::Video,
                    group,
                    captured_us: sender.clock_us(captured),
                    encoded_us: sender.clock_us(encoded),
                    encrypted_us: sender.clock_us(encrypted),
                    sent_us: sender.clock_us(sent_at),
                };
                for control in &target.controls {
                    let _ = control.send(&timing);
                }
            }
        }
        group = group.wrapping_add(1);
        sent += 1;
        if frames.is_some_and(|limit| sent >= limit) {
            shared.finish(None);
            break;
        }
    }
    Ok(())
}

/// Estado compartido por las tareas del anfitrión.
struct HostCtx {
    core: Arc<Core>,
    config: RemoteConfig,
    /// Vídeo de la sala; la resolución máxima y los fps cambian con [`Command::Video`].
    video: std::sync::Mutex<LoopbackConfig>,
    endpoint: Endpoint,
    signal: SignalSender,
    links: Arc<std::sync::Mutex<Links>>,
    encode: Arc<EncodeControl>,
    /// Límite de los intra forzados por `KeyframeRequest`.
    gate: Arc<KeyframeGate>,
    pending: std::sync::Mutex<Option<PendingEpoch>>,
    /// Datos del relay de la sala; `None` si el servidor no lo da (cuota agotada o sin relay).
    relay_info: std::sync::Mutex<Option<RelayInfo>>,
    relay_lock: tokio::sync::Mutex<()>,
    hello: Hello,
    /// Codificadores del anfitrión (lo que puede elegir; cambian con [`Command::Video`]).
    encoders: std::sync::Mutex<EncoderSet>,
    /// Bitrate máximo en kbit/s (`video.bitrate_kbps` al abrir; cambia con [`Command::Video`]).
    max_kbps: AtomicU32,
    /// Último aviso de por qué el vídeo no va en el formato pedido.
    format_note: std::sync::Mutex<Option<String>>,
    /// Lo que decodifica cada invitado conectado, cuando ya lo dijo (o venció la espera).
    guest_caps: std::sync::Mutex<BTreeMap<GuestId, GuestCapabilities>>,
    /// Códec y backend en uso; el cerrojo serializa las renegociaciones.
    current: tokio::sync::Mutex<Option<(Codec, Backend, VideoFormat)>>,
    /// Entrada de los invitados.
    input: Arc<HostInput>,
    /// Pistas de audio que se envían.
    audio_info: AudioInfo,
    /// Lo último que se anunció con `Sharing` (si se comparte algo).
    announced_sharing: AtomicBool,
}

/// `name` en como mucho [`MEMBER_NAME_MAX`] bytes, sin cortar un carácter.
pub(crate) fn truncate_name(name: &str) -> String {
    let mut end = name.len().min(MEMBER_NAME_MAX);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

/// Chat: la línea que ve la interfaz a partir de la entrada del historial.
fn chat_line(entry: ChatEntry, own: bool) -> ChatLine {
    ChatLine {
        author: ChatAuthor {
            guest: entry.guest,
            leaf: entry.leaf,
            name: entry.name,
        },
        text: entry.text,
        at_unix_ms: entry.at_unix_ms,
        own,
    }
}

/// Aviso cuando el servidor no tiene relay.
pub(crate) const RELAY_DISABLED: &str = "el servidor no tiene relay: la sala solo funciona en \
                                         directo";

impl HostCtx {
    /// El servidor quitó el relay a la sala (cuota agotada; él mismo corta sus conexiones).
    fn relay_unavailable(&self, message: String) {
        lock(&self.relay_info).take();
        self.core.emit(SessionEvent::RelayUnavailable(message));
    }

    /// El vídeo de ahora (resolución máxima y fps incluidos).
    fn video(&self) -> LoopbackConfig {
        *lock(&self.video)
    }

    /// Tamaño del flujo que se envía (lo que anuncian `HostFrameSize` y `CodecSelected`): el de la
    /// captura reducido al tope si no cabe ([`LoopbackConfig::stream_size`]); antes del primer
    /// fotograma, el tope.
    fn stream_size(&self) -> (u32, u32) {
        let video = self.video();
        match self.core.shared.captured_size() {
            Some(captured) => video.stream_size(captured),
            None => (video.width, video.height),
        }
    }

    /// Si la fuente cambió entre compartir algo y no compartir nada desde el último anuncio,
    /// avisa a todos los invitados conectados (`Sharing`).
    fn announce_sharing(&self) {
        let active = self.core.shared.sharing();
        if self.announced_sharing.swap(active, Ordering::AcqRel) == active {
            return;
        }
        debug!(active, "se comparte algo o ya no");
        let message = ControlMessage::Sharing { active };
        for (_, control) in lock(&self.links).guest_controls() {
            let _ = control.send(&message);
        }
    }

    /// Anuncia el tamaño del flujo a todos los invitados conectados.
    fn announce_frame_size(&self, (width, height): (u32, u32)) {
        let message = ControlMessage::HostFrameSize { width, height };
        for (_, control) in lock(&self.links).guest_controls() {
            let _ = control.send(&message);
        }
    }

    fn sender_config(&self) -> MediaSenderConfig {
        let max = self.max_kbps.load(Ordering::Acquire).max(1);
        MediaSenderConfig {
            frame_interval: frame_interval(self.video().fps),
            rate: RateConfig {
                min_kbps: super::min_kbps(max),
                max_kbps: max,
                // 8 Mbit/s hasta que el primer `Feedback` mida el enlace: arrancar en el máximo (o
                // en su mitad) llenaba la cola de un enlace más lento con el primer intra, el más
                // grande ([`super::start_kbps`]).
                start_kbps: super::start_kbps(max),
            },
            pacing: self.config.pacing,
        }
    }

    /// Otros codificadores, bitrate máximo, resolución máxima o fps con la sala abierta. El
    /// bitrate va derecho al control de tasa; lo demás reabre el codificador (un intra) con
    /// [`renegotiate`], y los invitados siguen: el tamaño nuevo les llega en el flujo (su
    /// decodificador lo toma del intra) y en `CodecSelected`, y los fps nuevos ajustan sus plazos.
    fn change_video(self: &Arc<Self>, change: VideoChange) {
        if let Some(max) = change.max_kbps {
            let max = max.max(1);
            self.max_kbps.store(max, Ordering::Release);
            for sender in lock(&self.links).senders() {
                sender.set_max_kbps(max);
            }
            debug!(kbps = max, "bitrate máximo");
        }
        let mut reopen = false;
        if change.max_size.is_some() || change.fps.is_some() {
            let captured = self.core.shared.captured_size();
            let (before, video) = {
                let mut video = lock(&self.video);
                let before = *video;
                if let Some((width, height)) = change.max_size {
                    video.width = width;
                    video.height = height;
                }
                if let Some(fps) = change.fps {
                    video.fps = fps.max(1);
                }
                (before, *video)
            };
            if video != before {
                reopen = true;
                debug!(
                    width = video.width,
                    height = video.height,
                    fps = video.fps,
                    "resolución máxima y fps"
                );
                // La captura en marcha (y la que se adopte después con `switch_capture`).
                let options = video.capture_options();
                if let Some(setup) = lock(&self.core.capture_setup).as_mut() {
                    setup.1 = options;
                }
                self.core.shared.set_capture_options(options);
                let links = lock(&self.links);
                if video.fps != before.fps {
                    let interval = frame_interval(video.fps);
                    for sender in links.senders() {
                        sender.set_frame_interval(interval);
                    }
                }
                // El tamaño que ven los invitados es el de la captura reducida al tope nuevo (el
                // hilo `encode` vuelve a avisar si la captura cambia después, p. ej. la sintética).
                let stream = |video: &LoopbackConfig| match captured {
                    Some(captured) => video.stream_size(captured),
                    None => (video.width, video.height),
                };
                let (width, height) = stream(&video);
                if (width, height) != stream(&before) {
                    let message = ControlMessage::HostFrameSize { width, height };
                    for (_, control) in links.guest_controls() {
                        let _ = control.send(&message);
                    }
                }
                drop(links);
                self.warn_over_capabilities(video);
            }
        }
        if let Some(encoders) = change.encoders {
            *lock(&self.encoders) = encoders;
            reopen = true;
        }
        if reopen {
            let ctx = Arc::clone(self);
            tokio::spawn(async move {
                // Sin códec actual, la negociación vuelve a abrir aunque salga el mismo códec:
                // así un cambio solo de backend, de tamaño o de fps también se aplica.
                ctx.current.lock().await.take();
                renegotiate(&ctx, None).await;
            });
        }
    }

    /// Avisa de los invitados que admiten menos que lo que se envía.
    fn warn_over_capabilities(&self, video: LoopbackConfig) {
        let (w, h, fps) = (video.width, video.height, video.fps);
        let over: Vec<String> = lock(&self.guest_caps)
            .iter()
            .filter(|(_, caps)| w > caps.max_width || h > caps.max_height || fps > caps.max_fps)
            .map(|(guest, caps)| {
                format!(
                    "el invitado {} admite hasta {}×{} a {} fps",
                    guest.0, caps.max_width, caps.max_height, caps.max_fps
                )
            })
            .collect();
        for note in over {
            self.core.warn(format!("{note} y se envía {w}×{h} a {fps}"));
        }
    }

    /// Instala la época pendiente (la del último invitado añadido) y actualiza las cabeceras.
    fn install_pending(&self) {
        let pending = lock(&self.pending).take();
        if let Some((epoch, keys)) = pending {
            self.install(epoch, &keys);
        }
    }

    fn install(&self, epoch: u64, keys: &[(u32, MediaKey)]) {
        if self.core.install_epoch(epoch, keys) {
            for sender in lock(&self.links).senders() {
                sender.set_epoch_hint(epoch as u16);
            }
        }
    }

    fn update_active(&self) {
        let connected = lock(&self.links).connected();
        self.encode.active.store(connected > 0, Ordering::Release);
    }

    /// Tras conectar un invitado: objetivo actual, evento y, si en [`CAPABILITIES_TIMEOUT`] no
    /// dice qué decodifica, H.264 por software. El intra para él no se pide aquí: su decodificador
    /// no está abierto hasta que lleguen sus capacidades y reciba `CodecSelected`, y lo pide
    /// [`renegotiate`].
    fn after_attach(
        self: &Arc<Self>,
        guest: GuestId,
        path: Path,
        remote: std::net::SocketAddr,
        control: &ControlSender,
    ) {
        self.update_active();
        let kbps = self.encode.bitrate_kbps.load(Ordering::Acquire);
        let _ = control.send(&ControlMessage::TargetBitrate { kbps });
        let permissions = lock(&self.core.guests).get_mut(&guest).map(|status| {
            status.path = Some(path);
            status.remote = Some(remote);
            status.permissions
        });
        // Entrada: el estado del hilo `inject` y lo que el invitado necesita para capturar.
        if let Some(permissions) = permissions {
            self.input.job(Job::Permissions { guest, permissions });
            let _ = control.send(&permissions_message(permissions));
        }
        let (width, height) = self.stream_size();
        let _ = control.send(&ControlMessage::HostFrameSize { width, height });
        let _ = control.send(&ControlMessage::Sharing {
            active: self.core.shared.sharing(),
        });
        // Audio: formato de cada pista y permiso de voz.
        for (track, format, config) in [
            (
                Track::Audio,
                coroom_audio::AudioFormat::GAME,
                self.audio_info.game,
            ),
            (
                Track::Voice,
                coroom_audio::AudioFormat::VOICE,
                self.audio_info.voice,
            ),
        ] {
            if let Some((bitrate, frame_ms)) = config {
                let _ = control.send(&ControlMessage::AudioConfig {
                    track,
                    sample_rate: format.sample_rate_hz,
                    channels: format.channels as u8,
                    frame_ms,
                    bitrate,
                });
            }
        }
        if let Some(permissions) = permissions {
            let _ = control.send(&ControlMessage::VoicePermission {
                allowed: permissions.voice,
            });
        }
        // Quién habla ya (el anfitrión puede llevar un rato hablando).
        for who in self.core.audio.talkers() {
            if who != guest.0 {
                let _ = control.send(&ControlMessage::VoiceState {
                    guest: who,
                    speaking: true,
                });
            }
        }
        // Chat: lo último que se escribió, recifrado con la época actual (la suya).
        self.send_chat_history(guest, control);
        self.core.emit(SessionEvent::GuestConnected {
            guest,
            path,
            remote,
        });
        self.broadcast_members();
        let ctx = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(CAPABILITIES_TIMEOUT).await;
            let missing =
                lock(&ctx.links).contains(guest) && !lock(&ctx.guest_caps).contains_key(&guest);
            if missing {
                ctx.core.warn(format!(
                    "el invitado {} no dijo qué decodifica en {} s; se usa H.264",
                    guest.0,
                    CAPABILITIES_TIMEOUT.as_secs()
                ));
                lock(&ctx.guest_caps).insert(guest, GuestCapabilities::h264_fallback());
                renegotiate(&ctx, Some(guest)).await;
            }
        });
    }

    /// Quita el enlace del invitado `guest` (si es el de `sender`).
    fn detach(&self, guest: GuestId, sender: &Arc<MediaSender>) {
        {
            let mut links = lock(&self.links);
            if links
                .direct
                .get(&guest)
                .is_some_and(|link| Arc::ptr_eq(&link.sender, sender))
            {
                links.direct.remove(&guest);
            }
            if let Some(relay) = &mut links.relay
                && Arc::ptr_eq(&relay.sender, sender)
                && relay.guests.remove(&guest).is_some()
            {
                relay.sender.forget_source(u64::from(guest.0));
            }
        }
        if let Some(status) = lock(&self.core.guests).get_mut(&guest) {
            status.path = None;
            status.rtt_ms = None;
            status.target_kbps = None;
        }
        lock(&self.guest_caps).remove(&guest);
        self.voice_state(guest, false);
        // Sin conexión no hay entrada: se suelta lo suyo y se libera su ranura.
        self.input.job(Job::GuestGone(guest));
        self.update_active();
    }

    /// Envía a `guest` sus permisos de entrada y de voz actuales.
    /// Un mensaje de control solo para `guest`.
    /// Manda a todos los invitados quién está en la sala (`Members`), para que cada uno silencie
    /// a quien quiera. El anfitrión va sin nombre: el invitado ya lo tiene de la señalización.
    fn broadcast_members(&self) {
        let members: Vec<WireMember> = std::iter::once(WireMember {
            guest: 0,
            leaf: self.core.own_leaf.load(Ordering::Acquire),
            name: String::new(),
        })
        .chain(lock(&self.core.guests).values().map(|status| WireMember {
            guest: status.guest.0,
            leaf: status.leaf,
            name: truncate_name(&status.name),
        }))
        .take(MEMBERS_MAX)
        .collect();
        let message = ControlMessage::Members { members };
        for (_, control) in lock(&self.links).guest_controls() {
            let _ = control.send(&message);
        }
    }

    fn send_to_guest(&self, guest: GuestId, message: &ControlMessage) {
        let controls = lock(&self.links).guest_controls();
        for (id, control) in controls {
            if id == guest {
                let _ = control.send(message);
            }
        }
    }

    fn send_permissions(&self, guest: GuestId) {
        let Some(permissions) = lock(&self.core.guests)
            .get(&guest)
            .map(|status| status.permissions)
        else {
            return;
        };
        let controls = lock(&self.links).guest_controls();
        for (id, control) in controls {
            if id == guest {
                let _ = control.send(&permissions_message(permissions));
                let _ = control.send(&ControlMessage::VoicePermission {
                    allowed: permissions.voice,
                });
            }
        }
        if !permissions.voice {
            self.voice_state(guest, false);
        }
    }

    /// `guest` empieza o deja de hablar: se anota y se avisa a los demás invitados.
    fn voice_state(&self, guest: GuestId, speaking: bool) {
        let was = self.core.audio.talkers().contains(&guest.0);
        if was == speaking {
            return;
        }
        self.core.audio.set_talker(guest.0, speaking);
        let message = ControlMessage::VoiceState {
            guest: guest.0,
            speaking,
        };
        for (id, control) in lock(&self.links).guest_controls() {
            if id != guest {
                let _ = control.send(&message);
            }
        }
    }

    /// Chat: un mensaje sellado a todos los invitados conectados (el autor incluido). Se llama con
    /// el chat bloqueado, así salen en el orden de sus contadores.
    fn chat_broadcast(&self, message: &ControlMessage) {
        for (_, control) in lock(&self.links).guest_controls() {
            let _ = control.send(message);
        }
    }

    /// Chat: lo que escribe el anfitrión (ya validado y contado en `send_chat`). El evento local
    /// sale con el chat bloqueado, como el reparto: así su orden es el del historial.
    fn chat_from_host(&self, text: String) {
        let leaf = self.core.own_leaf.load(Ordering::Acquire);
        let entry = ChatEntry {
            guest: 0,
            leaf,
            name: String::new(),
            text,
            at_unix_ms: 0,
        };
        let mut chat = lock(&self.core.chat);
        match chat.post(entry, leaf) {
            Ok((message, entry)) => {
                self.chat_broadcast(&message);
                self.core
                    .emit(SessionEvent::ChatMessage(chat_line(entry, true)));
            }
            Err(err) => {
                drop(chat);
                self.core
                    .warn(format!("no se pudo enviar el mensaje del chat: {err}"));
            }
        }
    }

    /// Chat: un mensaje del invitado `guest`: se abre, se valida (hoja, texto y ritmo), se le pone
    /// autor y hora, se guarda y se reparte a todos.
    fn chat_from_guest(&self, guest: GuestId, sealed: &SealedChat) {
        let Some((leaf, name)) = lock(&self.core.guests)
            .get(&guest)
            .map(|status| (status.leaf, status.name.clone()))
        else {
            return;
        };
        let own_leaf = self.core.own_leaf.load(Ordering::Acquire);
        let mut chat = lock(&self.core.chat);
        let posted = chat
            .accept_from_guest(guest.0, leaf, &name, sealed, Instant::now())
            .and_then(|entry| chat.post(entry, own_leaf));
        match posted {
            Ok((message, entry)) => {
                self.chat_broadcast(&message);
                self.core
                    .emit(SessionEvent::ChatMessage(chat_line(entry, false)));
            }
            Err(reason) => {
                drop(chat);
                debug!(guest = guest.0, %reason, "mensaje del chat descartado");
                self.core.emit(SessionEvent::ChatRejected {
                    reason: format!("el de {name} (invitado {}): {reason}", guest.0),
                });
            }
        }
    }

    /// Chat: vaciar para todos.
    fn chat_clear(&self) {
        let own_leaf = self.core.own_leaf.load(Ordering::Acquire);
        let mut chat = lock(&self.core.chat);
        match chat.clear(own_leaf) {
            Ok(message) => {
                self.chat_broadcast(&message);
                self.core.emit(SessionEvent::ChatCleared);
            }
            Err(err) => {
                drop(chat);
                self.core.warn(format!("no se pudo vaciar el chat: {err}"));
            }
        }
    }

    /// Chat: el historial al invitado que acaba de conectar.
    fn send_chat_history(&self, guest: GuestId, control: &ControlSender) {
        let own_leaf = self.core.own_leaf.load(Ordering::Acquire);
        let mut chat = lock(&self.core.chat);
        match chat.history_message(own_leaf) {
            Ok(message) => {
                let _ = control.send(&message);
            }
            Err(err) => debug!(guest = guest.0, %err, "historial del chat sin enviar"),
        }
    }

    fn is_member(&self, guest: GuestId) -> bool {
        lock(&self.core.guests).contains_key(&guest)
    }

    fn guest_by_key(&self, key: &PublicKey) -> Option<GuestId> {
        lock(&self.core.guests)
            .values()
            .find(|status| status.public_key == *key)
            .map(|status| status.guest)
    }
}

/// Conecta con el invitado: directo o, si no se puede, deja que llegue por el relay. La conexión
/// con el relay se abre a la vez que la apertura simultánea (si la sala tiene relay): cuando la
/// apertura falla, el invitado ya está autenticado en el relay y su control llega sin sumar las
/// idas y vueltas del handshake a los 3 s de la apertura. Los candidatos que el invitado anuncie
/// después (la WAN que abre su router) entran por `extra`.
async fn connect_guest(
    ctx: Arc<HostCtx>,
    guest: GuestId,
    key: PublicKey,
    addrs: Vec<std::net::SocketAddr>,
    extra: ExtraCandidates,
) {
    if ctx.config.force_relay {
        ensure_relay(&ctx).await;
        return;
    }
    if lock(&ctx.relay_info).is_some() {
        // En paralelo y sin avisar si falla: si gana la directa no hacía falta; si no, el intento
        // de abajo lo repite y avisa.
        let relay_ctx = Arc::clone(&ctx);
        tokio::spawn(async move { ensure_relay_with(&relay_ctx, true).await });
    }
    let connector = Connector::new(ctx.endpoint.clone()).with_timeout(ctx.config.punch_timeout);
    let attempt = connector.try_direct_with(key, &addrs, Some(extra)).await;
    let relay_reason = attempt.connection.is_none().then(|| attempt.relay_reason());
    if let Some(status) = lock(&ctx.core.guests).get_mut(&guest) {
        status.candidates = attempt.candidates.clone();
    }
    ctx.core.emit(SessionEvent::Candidates {
        guest: Some(guest),
        candidates: attempt.candidates,
        relay_reason: relay_reason.clone(),
    });
    let Some(connection) = attempt.connection else {
        if lock(&ctx.relay_info).is_none() {
            ctx.core.warn(format!(
                "el invitado {} no conecta en directo y la sala no tiene relay",
                guest.0
            ));
            return;
        }
        debug!(guest = guest.0, reason = ?relay_reason, "sin conexión directa; se usa el relay");
        ensure_relay(&ctx).await;
        return;
    };
    if lock(&ctx.links).contains(guest) || !ctx.is_member(guest) {
        connection.close("conexión duplicada");
        return;
    }
    drop_idle_relay(&ctx);
    match tokio::time::timeout(REPLY_TIMEOUT, connection.accept_control(ctx.hello.clone())).await {
        Ok(Ok(control)) if control.peer_hello().identity_pk == key => {
            attach_direct(&ctx, guest, connection, control);
        }
        other => {
            let reason = match other {
                Ok(Ok(_)) => "el control no es del invitado".to_owned(),
                Ok(Err(err)) => err.to_string(),
                Err(_) => "el invitado no abrió el control a tiempo".to_owned(),
            };
            ctx.core.warn(format!("invitado {}: {reason}", guest.0));
            connection.close("control");
        }
    }
}

/// `InputPermissions` de unos permisos.
fn permissions_message(permissions: Permissions) -> ControlMessage {
    ControlMessage::InputPermissions {
        keyboard_mouse: permissions.keyboard_mouse,
        gamepad_slot: permissions.gamepad_slot,
    }
}

fn attach_direct(
    ctx: &Arc<HostCtx>,
    guest: GuestId,
    connection: PeerConnection,
    control: ControlStream,
) {
    let (reply, receiver, _hello) = control.split();
    let sender = Arc::new(MediaSender::with_clock_origin(
        connection.clone(),
        ctx.sender_config(),
        ctx.core.clock_origin,
    ));
    ctx.input
        .spawn_readers(&ctx.core, &connection, Some(guest), &sender);
    voice_reader(ctx, connection.clone(), Some(guest));
    tokio::spawn(control_datagrams(
        Arc::clone(ctx),
        connection.clone(),
        Arc::clone(&sender),
    ));
    ctx.install_pending();
    sender.set_epoch_hint(ctx.core.epoch.load(Ordering::Acquire) as u16);
    lock(&ctx.links).direct.insert(
        guest,
        DirectLink {
            sender: Arc::clone(&sender),
            control: reply.clone(),
        },
    );
    ctx.after_attach(guest, Path::Direct, connection.remote_address(), &reply);
    tokio::spawn(probe_link(Arc::clone(ctx), Arc::clone(&sender)));
    tokio::spawn(control_reader(
        Arc::clone(ctx),
        guest,
        receiver,
        sender,
        reply,
        None,
    ));
}

/// Sonda del enlace hacia un emisor recién conectado: [`PROBE_TRAINS`] trenes a
/// [`probe_pace_kbps`] del máximo configurado (varias veces el máximo, hasta el techo del emisor:
/// la dispersión solo mide cuellos por debajo del ritmo de salida, y el pacing necesita saber
/// cuánto sobra por encima del máximo), antes de que el invitado pida el vídeo. Con lo que informe
/// arranca su control de tasa ([`MediaSender::on_probe_result`], acotado al máximo) y se fija el
/// ritmo del pacing ([`MediaSender::pacing_plan`], que no se acota al máximo).
async fn probe_link(ctx: Arc<HostCtx>, sender: Arc<MediaSender>) {
    let pace = probe_pace_kbps(ctx.max_kbps.load(Ordering::Acquire));
    for train in 0..PROBE_TRAINS {
        if train > 0 {
            tokio::time::sleep(PROBE_SPACING).await;
        }
        if let Err(err) = sender.send_probe(pace).await {
            debug!(error = %err, "no se pudo enviar la sonda del enlace");
            return;
        }
    }
}

/// Conecta con el relay una sola vez y atiende los controles que abre cada invitado por él.
async fn ensure_relay(ctx: &Arc<HostCtx>) {
    ensure_relay_with(ctx, false).await;
}

/// Cierra la conexión con el relay si no tiene ningún invitado: la que se abrió en paralelo con
/// una apertura simultánea que acabó en directo. Una sesión solo en LAN no mantiene así una
/// conexión ociosa con el relay; si otro invitado la necesita, [`ensure_relay`] la vuelve a abrir.
fn drop_idle_relay(ctx: &Arc<HostCtx>) {
    let idle = {
        let mut links = lock(&ctx.links);
        match &links.relay {
            Some(relay) if relay.guests.is_empty() => links.relay.take(),
            _ => None,
        }
    };
    if let Some(relay) = idle {
        debug!("relay sin invitados: se cierra (hay conexión directa)");
        relay.sender.connection().close("hay conexión directa");
    }
}

/// Conecta con el relay si la sala lo tiene y aún no está. Con `quiet`, un fallo solo se anota
/// en el log (es el intento en paralelo con la apertura directa).
async fn ensure_relay_with(ctx: &Arc<HostCtx>, quiet: bool) {
    let _guard = ctx.relay_lock.lock().await;
    if lock(&ctx.links).relay.is_some() {
        return;
    }
    let Some(relay_info) = lock(&ctx.relay_info).clone() else {
        debug!("la sala no tiene relay; solo conexiones directas");
        return;
    };
    let connection = match connect::connect_relay(&ctx.endpoint, &relay_info).await {
        Ok(connection) => connection,
        Err(err) if quiet => {
            debug!(error = %err, "no se pudo abrir el relay en paralelo");
            return;
        }
        Err(err) => {
            ctx.core
                .warn(format!("no se pudo conectar con el relay: {err}"));
            return;
        }
    };
    let sender = Arc::new(MediaSender::with_clock_origin(
        connection.clone(),
        ctx.sender_config(),
        ctx.core.clock_origin,
    ));
    sender.set_epoch_hint(ctx.core.epoch.load(Ordering::Acquire) as u16);
    // Por el relay llegan todos los invitados: los lectores separan por la clave de cada uno.
    ctx.input
        .spawn_readers(&ctx.core, &connection, None, &sender);
    voice_reader(ctx, connection.clone(), None);
    tokio::spawn(control_datagrams(
        Arc::clone(ctx),
        connection.clone(),
        Arc::clone(&sender),
    ));
    lock(&ctx.links).relay = Some(RelayLink {
        sender: Arc::clone(&sender),
        guests: BTreeMap::new(),
    });
    tokio::spawn(relay_accept_loop(Arc::clone(ctx), connection, sender));
}

async fn relay_accept_loop(
    ctx: Arc<HostCtx>,
    connection: PeerConnection,
    sender: Arc<MediaSender>,
) {
    loop {
        if connection.quinn().close_reason().is_some() || ctx.core.ended() {
            break;
        }
        let control = match connection.accept_control(ctx.hello.clone()).await {
            Ok(control) => control,
            Err(coroom_transport::TransportError::Closed) => break,
            Err(err) => {
                debug!(error = %err, "control del relay rechazado");
                continue;
            }
        };
        let key = control.peer_hello().identity_pk;
        let Some(guest) = ctx.guest_by_key(&key) else {
            debug!("control del relay de un invitado desconocido");
            continue;
        };
        if lock(&ctx.links).contains(guest) {
            continue;
        }
        let (reply, receiver, _hello) = control.split();
        ctx.install_pending();
        sender.set_epoch_hint(ctx.core.epoch.load(Ordering::Acquire) as u16);
        if let Some(relay) = &mut lock(&ctx.links).relay {
            relay.guests.insert(guest, reply.clone());
        }
        ctx.after_attach(
            guest,
            Path::Relayed,
            sender.connection().remote_address(),
            &reply,
        );
        // Por el relay el tren llega a todos los invitados; cada uno informa el suyo.
        tokio::spawn(probe_link(Arc::clone(&ctx), Arc::clone(&sender)));
        tokio::spawn(control_reader(
            Arc::clone(&ctx),
            guest,
            receiver,
            Arc::clone(&sender),
            reply,
            Some(u64::from(guest.0)),
        ));
    }
    let mut links = lock(&ctx.links);
    if links
        .relay
        .as_ref()
        .is_some_and(|relay| Arc::ptr_eq(&relay.sender, &sender))
    {
        links.relay = None;
    }
    drop(links);
    ctx.update_active();
    debug!("conexión con el relay cerrada");
}

/// Atiende las copias en datagrama de los `Nack`, `KeyframeRequest` y `Feedback` de una conexión
/// (directa o la del relay, que las trae de todos sus invitados): llegan antes que las del stream
/// de control cuando este perdió un paquete. Las repetidas no reenvían ni aplican nada
/// (`MediaSender::on_nack`, `KeyframeGate`, `MediaSender::on_feedback_from`). Por el relay, el
/// `Feedback` se atribuye por la etiqueta del datagrama (el id de par del invitado), que tiene
/// que ser el de un invitado conectado por el relay.
async fn control_datagrams(
    ctx: Arc<HostCtx>,
    connection: PeerConnection,
    sender: Arc<MediaSender>,
) {
    let relayed = connection.path() == Path::Relayed;
    while let Ok(datagram) = connection.read_control_datagram().await {
        match coroom_transport::proto::control::parse_control_datagram_tagged(&datagram) {
            Ok((
                _,
                ControlMessage::Nack {
                    track,
                    group,
                    missing,
                },
            )) => {
                sender.on_nack(track, group, &missing);
            }
            Ok((_, ControlMessage::KeyframeRequest { group })) => {
                keyframe_request(&ctx, group, connection.rtt());
            }
            Ok((tag, ControlMessage::Feedback(feedback))) => {
                if !relayed {
                    sender.on_feedback(&feedback);
                    continue;
                }
                let guest = GuestId(tag);
                let known = lock(&ctx.links)
                    .relay
                    .as_ref()
                    .is_some_and(|relay| relay.guests.contains_key(&guest));
                if known {
                    sender.on_feedback_from(u64::from(tag), &feedback);
                }
            }
            Ok(_) => {}
            Err(err) => debug!(error = %err, "datagrama de control no válido"),
        }
    }
}

/// Lee el control de un invitado: `Feedback`, `Nack` y `Ping` los atiende el emisor; un
/// `KeyframeRequest` pide un intra.
async fn control_reader(
    ctx: Arc<HostCtx>,
    guest: GuestId,
    mut receiver: ControlReceiver,
    sender: Arc<MediaSender>,
    reply: ControlSender,
    source: Option<u64>,
) {
    loop {
        let message = match receiver.recv().await {
            Ok(Some(message)) => message,
            Ok(None) | Err(_) => break,
        };
        let rest = match source {
            Some(source) => sender.handle_control_from(source, message, &reply),
            None => sender.handle_control(message, &reply),
        };
        match rest {
            Some(ControlMessage::KeyframeRequest { group }) => {
                let rtt = sender.connection().rtt();
                keyframe_request(&ctx, group, rtt);
            }
            Some(ControlMessage::Capabilities(caps)) => {
                let caps = GuestCapabilities::from_wire(&caps);
                debug!(guest = guest.0, ?caps, "capacidades del invitado");
                let video = ctx.video();
                let (w, h, fps) = (video.width, video.height, video.fps);
                if w > caps.max_width || h > caps.max_height || fps > caps.max_fps {
                    ctx.core.warn(format!(
                        "el invitado {} admite hasta {}×{} a {} fps y se envía {w}×{h} a {fps}",
                        guest.0, caps.max_width, caps.max_height, caps.max_fps
                    ));
                }
                lock(&ctx.guest_caps).insert(guest, caps);
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move { renegotiate(&ctx, Some(guest)).await });
            }
            Some(ControlMessage::VoiceState { speaking, .. }) => {
                let allowed = lock(&ctx.core.guests)
                    .get(&guest)
                    .is_some_and(|status| status.permissions.voice);
                ctx.voice_state(guest, speaking && allowed);
            }
            Some(ControlMessage::ProbeResult { .. }) => {
                // Ya lo aplicó el emisor; se anota lo mejor medido hacia este invitado.
                if let Some(outcome) = sender.probe(source.unwrap_or(DEFAULT_SOURCE))
                    && let Some(kbps) = outcome.kbps()
                {
                    let changed = lock(&ctx.core.guests)
                        .get_mut(&guest)
                        .is_some_and(|status| status.link_kbps.replace(kbps) != Some(kbps));
                    if changed {
                        debug!(
                            guest = guest.0,
                            kbps,
                            lost = outcome.lost(),
                            "sonda del enlace"
                        );
                        ctx.core.emit(SessionEvent::LinkProbe {
                            guest: Some(guest),
                            kbps,
                            lost: outcome.lost(),
                            sent: outcome.sent,
                        });
                    }
                }
            }
            Some(ControlMessage::Chat(sealed)) => ctx.chat_from_guest(guest, &sealed),
            Some(ControlMessage::HandoffReply { code }) => {
                // Un código que no es de sala cuenta como «no».
                let code =
                    code.filter(|code| coroom_transport::proto::signal::is_valid_room_code(code));
                ctx.core.emit(SessionEvent::HandoffReply { guest, code });
            }
            Some(ControlMessage::Bye) => break,
            _ => {}
        }
    }
    ctx.detach(guest, &sender);
    if source.is_none() {
        sender.connection().close("fin");
    }
    debug!(guest = guest.0, "control del invitado cerrado");
}

/// Elige el códec con lo que decodifican los invitados conectados que ya lo dijeron y su formato
/// ([`choose_format`]) y, si cambia algo, abre el codificador nuevo (fuera del runtime:
/// abrir un codificador de GPU tarda), se lo pasa al hilo `encode` y lo anuncia a todos con
/// `CodecSelected`. Si no cambia, solo se lo anuncia a `guest` (el que acaba de llegar). Si el
/// códec elegido no abre en ese formato, prueba el de siempre y luego los demás códecs comunes.
async fn renegotiate(ctx: &Arc<HostCtx>, guest: Option<GuestId>) {
    let mut current = ctx.current.lock().await;
    let named: Vec<(String, GuestCapabilities)> = {
        let caps = lock(&ctx.guest_caps);
        let links = lock(&ctx.links);
        let statuses = lock(&ctx.core.guests);
        caps.iter()
            .filter(|(id, _)| links.contains(**id))
            .map(|(id, caps)| {
                let name = statuses
                    .get(id)
                    .map_or_else(|| format!("el invitado {}", id.0), |s| s.name.clone());
                (name, caps.clone())
            })
            .collect()
    };
    let guests: Vec<GuestCapabilities> = named.iter().map(|(_, caps)| caps.clone()).collect();
    let decoders: Vec<_> = guests.iter().map(|caps| caps.decoders.clone()).collect();
    let encoders = lock(&ctx.encoders).clone();
    let mut available = encoders.available().to_vec();
    let video = ctx.video();
    let candidates = video.format_candidates();
    loop {
        let Some(codec) = choose_codec(&available, &decoders) else {
            if !guests.is_empty() {
                ctx.core.warn(
                    "no hay ningún códec que el anfitrión codifique y todos los invitados \
                     decodifiquen",
                );
            }
            return;
        };
        let format = choose_format(codec, &candidates, &encoders, &guests);
        // Si no va en el formato pedido (4:4:4, HDR), se dice por qué, una vez por motivo.
        let wanted = candidates.first().copied().unwrap_or(VideoFormat::SDR_420);
        let note = format_shortfall(codec, wanted, format, &encoders, &named);
        {
            let mut last = lock(&ctx.format_note);
            if note != *last {
                if let Some(note) = &note {
                    ctx.core.warn(note.clone());
                }
                *last = note;
            }
        }
        if current.is_some_and(|(c, _, f)| c == codec && f == format) {
            if let Some(guest) = guest {
                announce(ctx, codec, format, Some(guest));
                // El invitado acaba de abrir (o va a abrir) su decodificador: necesita un intra.
                // Con la pantalla quieta, el hilo `encode` repite el último fotograma.
                ctx.core.shared.request_keyframe();
            }
            return;
        }
        let mut formats = vec![format];
        if !format.is_default() {
            formats.push(VideoFormat::SDR_420);
        }
        for format in formats {
            let config = EncoderConfig {
                codec,
                width: video.width,
                height: video.height,
                fps: video.fps,
                bitrate_kbps: ctx.encode.bitrate_kbps.load(Ordering::Acquire),
                slices: DEFAULT_SLICES,
                format,
            };
            let encoders = encoders.clone();
            // Con varios backends para el códec (modo automático), el de menor latencia medida
            // (`EncoderSet::open_calibrated`: unas décimas de segundo la primera vez, fuera del
            // runtime); con uno (ajuste explícito), ese. Solo se mide sin codificador en marcha
            // (al abrir la sala, antes del primer invitado): con la pantalla ya en el aire, abrir
            // y medir varios codificadores de GPU la haría tartamudear, así que entonces se abre
            // lo ya calibrado en esta GPU sin medir (`open_remembered`; sin nada guardado, la
            // lista de preferencia).
            let calibrate = current.is_none();
            let opened = tokio::task::spawn_blocking(move || {
                if calibrate {
                    encoders.open_calibrated(&config)
                } else {
                    encoders.open_remembered(&config)
                }
            })
            .await;
            let err = match opened {
                Ok(Ok((encoder, calibrated))) => {
                    let backend = encoder.backend();
                    // La captura entrega lo que importa el codificador preferido (al abrir la
                    // sala); si el que se abrió no lee esas imágenes (NVENC con DMA-BUF, software
                    // con D3D11), la captura pasa a memoria de CPU antes del primer fotograma. Lo
                    // que se decide al codificar (importar o descargar) lo pide el hilo `encode`.
                    let capture = ctx.core.shared.latest_capture_gpu_frames().or_else(|| {
                        lock(&ctx.core.capture_setup)
                            .as_ref()
                            .map(|setup| setup.0.clone())
                    });
                    if let Some(capture) = capture
                        && let Some(frames) = capture_frames_for(&capture, &encoder.gpu_input())
                    {
                        debug!(
                            %backend,
                            ?capture,
                            "la captura pasa a memoria de CPU: el codificador no lee sus imágenes"
                        );
                        ctx.core.shared.set_capture_gpu_frames(frames);
                    }
                    let label = encoder_label(codec, backend);
                    // El formato solo se nombra si no es el de siempre (4:2:0 SDR).
                    let label = if format.is_default() {
                        label
                    } else {
                        format!("{label} · {format}")
                    };
                    let mut detail = format!("codifica {label}");
                    if let Some(calibrated) = &calibrated {
                        for result in &calibrated.results {
                            debug!(codec = %codec, medida = %result, "calibración del codificador");
                        }
                        detail.push_str(&format!(
                            " ({}calibrado: p95 {:.1} ms)",
                            if calibrated.from_cache {
                                ""
                            } else {
                                "recién "
                            },
                            calibrated.p95.as_secs_f64() * 1000.0
                        ));
                    }
                    if let Some(rate_control) = encoder.rate_control() {
                        debug!(codec = %codec, %rate_control, "control de tasa del codificador");
                        detail.push_str(&format!("; {rate_control}"));
                    }
                    ctx.core.ledger.set_video(format!("{codec} · {label}"));
                    ctx.encode.replace(encoder);
                    *current = Some((codec, backend, format));
                    ctx.core.emit(SessionEvent::VideoCodec { codec, detail });
                    announce(ctx, codec, format, None);
                    ctx.core.shared.request_keyframe();
                    return;
                }
                Ok(Err(err)) => err.to_string(),
                Err(err) => err.to_string(),
            };
            ctx.core.warn(format!(
                "no se pudo abrir el codificador {codec} en {format}: {err}"
            ));
        }
        available.retain(|(c, _)| *c != codec);
    }
}

/// Intervalo entre fotogramas a `fps` (al menos 1).
fn frame_interval(fps: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps.max(1)))
}

/// Atiende un `KeyframeRequest` del grupo `group` de un invitado con este RTT ([`KeyframeGate`]).
fn keyframe_request(ctx: &Arc<HostCtx>, group: u32, rtt: Duration) {
    let interval = frame_interval(ctx.video().fps);
    match ctx.gate.on_request(group, rtt, interval, Instant::now()) {
        KeyframeDecision::Now => ctx.core.shared.request_keyframe(),
        KeyframeDecision::Later(wait) => {
            bump(&ctx.core.shared.counters.coalesced_requests);
            let ctx = Arc::clone(ctx);
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                ctx.gate.fire_deferred(Instant::now());
                ctx.core.shared.request_keyframe();
            });
        }
        KeyframeDecision::Ignore => bump(&ctx.core.shared.counters.coalesced_requests),
    }
}

/// Envía `CodecSelected` a `guest` o, con `None`, a todos los conectados.
fn announce(ctx: &HostCtx, codec: Codec, format: VideoFormat, guest: Option<GuestId>) {
    let (chroma, color) = format_to_wire(format);
    let video = ctx.video();
    let (width, height) = ctx.stream_size();
    let message = ControlMessage::CodecSelected {
        codec: codec_to_wire(codec),
        width,
        height,
        fps: video.fps,
        chroma,
        color,
    };
    let controls = lock(&ctx.links).guest_controls();
    for (id, control) in controls {
        if guest.is_none_or(|guest| guest == id) {
            let _ = control.send(&message);
        }
    }
}

/// Envía el cursor a cada invitado como mucho a `CURSOR_MAX_HZ` y solo si cambia: cada posición
/// en un datagrama de control numerado por cada conexión (gana el último; una pérdida no retiene
/// a los siguientes) y una copia por el stream de control como mucho cada
/// [`CURSOR_STREAM_PERIOD`] (el respaldo si los datagramas no pasan). Por el stream iba cada
/// posición y una pérdida congelaba el cursor ~100 ms (el PTO) y después reproducía las posiciones
/// viejas. La forma, una vez por forma y por invitado, por el stream, antes de la primera posición
/// que la usa.
async fn cursor_loop(ctx: Arc<HostCtx>) {
    let mut tick = tokio::time::interval(CURSOR_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent: BTreeMap<GuestId, CursorSent> = BTreeMap::new();
    let mut seq: u32 = 0;
    let mut last_datagram: Option<CursorState> = None;
    loop {
        tick.tick().await;
        if ctx.core.ended() {
            break;
        }
        let cursor = *lock(&ctx.core.shared.cursor);
        let (controls, connections) = {
            let links = lock(&ctx.links);
            (links.guest_controls(), links.connections())
        };
        sent.retain(|guest, _| controls.iter().any(|(id, _)| id == guest));
        let Some(cursor) = cursor else {
            continue;
        };
        let shape = ctx.core.shared.cursor_shape(cursor.shape_id);
        let now = Instant::now();
        for (guest, control) in controls {
            let last = sent.entry(guest).or_default();
            if let Some(shape) = &shape
                && last.shape != Some(shape.shape_id)
                && let Some(message) = shape_to_message(shape)
                && control.send(&message).is_ok()
            {
                last.shape = Some(shape.shape_id);
            }
            let due = last
                .stream_at
                .is_none_or(|at| now.saturating_duration_since(at) >= CURSOR_STREAM_PERIOD);
            if last.cursor != Some(cursor) && due {
                let message = ControlMessage::Cursor {
                    x: cursor.x,
                    y: cursor.y,
                    visible: cursor.visible,
                    shape_id: cursor.shape_id,
                };
                if control.send(&message).is_ok() {
                    last.cursor = Some(cursor);
                    last.stream_at = Some(now);
                }
            }
        }
        if last_datagram != Some(cursor) && !connections.is_empty() {
            seq = seq.wrapping_add(1);
            let message = ControlMessage::Cursor {
                x: cursor.x,
                y: cursor.y,
                visible: cursor.visible,
                shape_id: cursor.shape_id,
            };
            if let Some(datagram) =
                coroom_transport::proto::control::control_datagram_tagged(&message, seq)
            {
                let datagram = Bytes::from(datagram);
                for connection in &connections {
                    let _ = connection.send_datagram(datagram.clone());
                }
            }
            last_datagram = Some(cursor);
        }
    }
}

/// Lo último que recibió un invitado del cursor por el stream de control ([`cursor_loop`]).
#[derive(Debug, Default)]
struct CursorSent {
    /// Última posición enviada por el stream.
    cursor: Option<CursorState>,
    /// Última forma enviada.
    shape: Option<u64>,
    /// Cuándo salió la última copia por el stream.
    stream_at: Option<Instant>,
}

/// Cada cuántos periodos de `rate_loop` se informa a cada invitado de su latencia de entrada.
const INPUT_REPORT_TICKS: u32 = 10;

/// Envía a cada invitado que haya mandado entrada su latencia y sus contadores (`InputReport`).
fn send_input_reports(ctx: &HostCtx, sent: &mut BTreeMap<GuestId, (u64, u64)>) {
    let reports: Vec<(GuestId, ControlMessage, (u64, u64))> = lock(&ctx.core.guests)
        .values()
        .filter(|status| status.input.applied + status.input.dropped > 0)
        .map(|status| {
            let input = status.input;
            let clamp = |us: u64| u32::try_from(us).unwrap_or(u32::MAX);
            (
                status.guest,
                ControlMessage::InputReport {
                    samples: u32::try_from(input.samples).unwrap_or(u32::MAX),
                    p50_us: clamp(input.p50_us),
                    p95_us: clamp(input.p95_us),
                    applied: input.applied,
                    dropped: input.dropped,
                    queue_p50_us: clamp(input.queue_p50_us),
                    network_p50_us: clamp(input.network_p50_us),
                    inject_p50_us: clamp(input.inject_p50_us),
                    repaired: input.repaired,
                },
                (input.applied, input.dropped),
            )
        })
        .collect();
    let controls = lock(&ctx.links).guest_controls();
    for (guest, message, counts) in reports {
        if sent.get(&guest) == Some(&counts) {
            continue;
        }
        if let Some((_, control)) = controls.iter().find(|(id, _)| *id == guest)
            && control.send(&message).is_ok()
        {
            sent.insert(guest, counts);
        }
    }
}

/// Recalcula el bitrate del codificador (mínimo de los invitados) y el estado de cada invitado.
async fn rate_loop(ctx: Arc<HostCtx>) {
    let mut tick = tokio::time::interval(RATE_PERIOD);
    let mut ticks = 0u32;
    let mut reported = BTreeMap::new();
    let mut stats = StatsWindow::new(&ctx.core.shared.counters, &ctx.core.ledger);
    loop {
        tick.tick().await;
        if ctx.core.ended() {
            break;
        }
        if stats.due() && ctx.encode.active.load(Ordering::Acquire) {
            let target = ctx.encode.bitrate_kbps.load(Ordering::Acquire);
            let line = stats.host_line(&ctx.core.shared.counters, &ctx.core.ledger, target);
            tracing::info!(target: "coroom::stats", "{line}");
        }
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(INPUT_REPORT_TICKS) {
            send_input_reports(&ctx, &mut reported);
        }
        let (min, controls, status) = {
            let links = lock(&ctx.links);
            (links.min_target(), links.controls(), links.status())
        };
        if let Some(target) = min {
            let previous = ctx.encode.bitrate_kbps.swap(target, Ordering::AcqRel);
            if previous != target {
                debug!(kbps = target, "bitrate del codificador");
                for control in &controls {
                    let _ = control.send(&ControlMessage::TargetBitrate { kbps: target });
                }
            }
        }
        let mut guests = lock(&ctx.core.guests);
        for link in status {
            if let Some(entry) = guests.get_mut(&link.guest) {
                entry.path = Some(link.path);
                entry.remote = Some(link.remote);
                entry.rtt_ms = Some(link.rtt_ms);
                entry.target_kbps = link.target_kbps;
            }
        }
    }
}

/// Miembro de la sala.
struct Member {
    key: PublicKey,
    leaf: u32,
}

/// La sala del anfitrión (solo la toca la tarea principal).
struct Room {
    ctx: Arc<HostCtx>,
    group: RoomGroup,
    code: String,
    candidates: Vec<std::net::SocketAddr>,
    members: BTreeMap<GuestId, Member>,
    /// Invitados con la apertura simultánea en marcha y el canal de sus candidatos tardíos.
    connecting: BTreeMap<GuestId, mpsc::UnboundedSender<Vec<std::net::SocketAddr>>>,
}

fn network(what: &str) -> impl Fn(coroom_transport::TransportError) -> SessionError + '_ {
    move |err| SessionError::Network(format!("{what}: {err}"))
}

fn crypto(what: &str) -> impl Fn(coroom_transport::crypto::MlsError) -> SessionError + '_ {
    move |err| SessionError::Crypto(format!("{what}: {err}"))
}

/// Envía por la señalización; un fallo aquí es que el servidor cerró la sesión.
async fn send(signal: &SignalSender, message: SignalMessage) -> Result<(), SessionError> {
    signal.send(&message).await.map_err(network("señalización"))
}

impl Room {
    async fn mls(&self, to: GuestId, payload: Vec<u8>) -> Result<(), SessionError> {
        send(
            &self.ctx.signal,
            SignalMessage::Mls {
                from_peer: 0,
                to_peer: Some(to.0),
                payload,
            },
        )
        .await
    }

    /// El router contestó: si abrió el puerto, la dirección WAN se añade a los candidatos y se
    /// vuelven a anunciar a todos los invitados de la sala (los que estén en la apertura
    /// simultánea la prueban enseguida).
    async fn announce_mapping(
        &mut self,
        report: &coroom_transport::nat::MappingReport,
        observed: std::net::SocketAddr,
    ) -> Result<(), SessionError> {
        let with_wan =
            advertised_candidates(&self.ctx.config, &self.ctx.endpoint, observed, Some(report));
        if with_wan == self.candidates {
            return Ok(());
        }
        self.candidates = with_wan;
        let members: Vec<GuestId> = self.members.keys().copied().collect();
        for guest in members {
            send(
                &self.ctx.signal,
                SignalMessage::Candidates {
                    from_peer: 0,
                    to_peer: guest.0,
                    addrs: self.candidates.clone(),
                },
            )
            .await?;
        }
        Ok(())
    }

    async fn on_signal(&mut self, message: SignalMessage) -> Result<(), SessionError> {
        match message {
            SignalMessage::PeerJoined {
                peer_id,
                identity_pk,
                key_package,
                user,
                name,
            } => {
                self.on_peer_joined(GuestId(peer_id), identity_pk, &key_package, user, name)
                    .await
            }
            SignalMessage::Invited {
                from_user,
                from_name,
                code,
            } => {
                self.ctx.core.emit(SessionEvent::Invited {
                    from_user,
                    from_name,
                    code,
                });
                Ok(())
            }
            SignalMessage::JoinRequested {
                from_user,
                from_name,
            } => {
                self.ctx.core.emit(SessionEvent::JoinRequested {
                    from_user,
                    from_name,
                });
                Ok(())
            }
            SignalMessage::JoinDeclined {
                from_user,
                from_name,
            } => {
                self.ctx.core.emit(SessionEvent::JoinDeclined {
                    from_user,
                    from_name,
                });
                Ok(())
            }
            SignalMessage::Activity { user, activity } => {
                self.ctx
                    .core
                    .emit(SessionEvent::Activity { user, activity });
                Ok(())
            }
            SignalMessage::Presence { user, online } => {
                self.ctx.core.emit(SessionEvent::Presence { user, online });
                Ok(())
            }
            SignalMessage::QuotaExceeded { message } => {
                self.ctx.relay_unavailable(message);
                Ok(())
            }
            SignalMessage::RelayDisabled => {
                self.ctx.relay_unavailable(RELAY_DISABLED.to_owned());
                Ok(())
            }
            SignalMessage::Candidates {
                from_peer, addrs, ..
            } => {
                let guest = GuestId(from_peer);
                let Some(member) = self.members.get(&guest) else {
                    return Ok(());
                };
                if let Some(extra) = self.connecting.get(&guest) {
                    // Un segundo anuncio (la WAN que abrió su router): a la apertura en marcha,
                    // si sigue (si no, el canal está cerrado y no pasa nada).
                    let _ = extra.send(addrs);
                    return Ok(());
                }
                let (extra_tx, extra_rx) = mpsc::unbounded_channel();
                self.connecting.insert(guest, extra_tx);
                tokio::spawn(connect_guest(
                    Arc::clone(&self.ctx),
                    guest,
                    member.key,
                    addrs,
                    extra_rx,
                ));
                Ok(())
            }
            SignalMessage::Mls {
                from_peer, payload, ..
            } => {
                self.on_mls(GuestId(from_peer), &payload);
                Ok(())
            }
            SignalMessage::PeerLeft { peer_id } => self.remove(GuestId(peer_id), false).await,
            SignalMessage::Error { code, message } => {
                match code {
                    // Un mensaje a un invitado que ya se fue: no pasa nada.
                    ErrorCode::PeerNotFound => debug!(%message, "par ya fuera de la sala"),
                    _ => self.ctx.core.warn(format!("el servidor dice: {message}")),
                }
                Ok(())
            }
            other => {
                debug!(?other, "mensaje de señalización ignorado");
                Ok(())
            }
        }
    }

    async fn on_peer_joined(
        &mut self,
        guest: GuestId,
        identity_pk: PublicKey,
        key_package: &[u8],
        user: u64,
        name: String,
    ) -> Result<(), SessionError> {
        let own = self.ctx.core.identity.public_key();
        if identity_pk == own {
            self.ctx.core.warn(format!(
                "el invitado {} usa la misma identidad que el anfitrión (¿mismo --data-dir?); se le \
                 rechaza",
                guest.0
            ));
            return send(&self.ctx.signal, SignalMessage::Kick { peer_id: guest.0 }).await;
        }
        let change = match self.group.add_member(key_package) {
            Ok(change) => change,
            Err(err) => {
                self.ctx
                    .core
                    .warn(format!("el invitado {} no pudo entrar: {err}", guest.0));
                return send(&self.ctx.signal, SignalMessage::Kick { peer_id: guest.0 }).await;
            }
        };
        let Some(added) = change.added else {
            return Err(SessionError::Crypto(
                "el commit no añadió al invitado".to_owned(),
            ));
        };
        if added.public_key != identity_pk {
            // La identidad que dice el servidor no es la de la credencial MLS firmada.
            self.ctx.core.warn(format!(
                "el invitado {} presenta una identidad distinta de la de su KeyPackage; se le \
                 rechaza",
                guest.0
            ));
            let removal = self
                .group
                .remove_member(added.leaf_index)
                .map_err(crypto("MLS"))?;
            self.ctx.install(
                removal.epoch,
                &self.group.media_keys().map_err(crypto("MLS"))?,
            );
            self.install_chat_key(true);
            return send(&self.ctx.signal, SignalMessage::Kick { peer_id: guest.0 }).await;
        }

        // Se le admite antes de darle los candidatos (ver `guest.rs`: un handshake rechazado
        // después de que el cliente lo diese por bueno rompería la apertura simultánea).
        self.ctx.endpoint.admit(identity_pk);
        // Commit a los que ya estaban, Welcome y clave X-Wing al nuevo, candidatos.
        let commit = Payload::Commit(&change.commit).encode();
        let existing: Vec<GuestId> = self.members.keys().copied().collect();
        for other in existing {
            self.mls(other, commit.clone()).await?;
        }
        let Some(welcome) = &change.welcome else {
            return Err(SessionError::Crypto("falta el Welcome".to_owned()));
        };
        self.mls(guest, Payload::Welcome(welcome).encode()).await?;
        match hpke::signed_pq_key(&self.ctx.core.identity) {
            Ok(key) => self.mls(guest, Payload::HostPqKey(&key).encode()).await?,
            Err(err) => self.ctx.core.warn(format!(
                "sin clave X-Wing para el invitado {}: {err}",
                guest.0
            )),
        }
        send(
            &self.ctx.signal,
            SignalMessage::Candidates {
                from_peer: 0,
                to_peer: guest.0,
                addrs: self.candidates.clone(),
            },
        )
        .await?;

        // La clave del chat de la época nueva se usa ya (quien aún no procesó el commit guarda lo
        // que llegue hasta tenerla); las anteriores se conservan: no ha salido nadie.
        self.install_chat_key(false);
        // La época nueva se instala cuando el invitado conecte (ver el módulo).
        let keys = self.group.media_keys().map_err(crypto("MLS"))?;
        *lock(&self.ctx.pending) = Some((change.epoch, keys));
        self.members.insert(
            guest,
            Member {
                key: identity_pk,
                leaf: added.leaf_index,
            },
        );
        let safety_number = SafetyNumber::between(&own, &identity_pk);
        let fingerprint = sha256_fingerprint(&identity_pk);
        let permissions = Permissions::on_entry(self.ctx.config.guests_muted);
        lock(&self.ctx.core.guests).insert(
            guest,
            GuestStatus {
                guest,
                user,
                name: name.clone(),
                public_key: identity_pk,
                fingerprint: fingerprint.clone(),
                safety_number,
                permissions,
                leaf: added.leaf_index,
                path: None,
                remote: None,
                rtt_ms: None,
                target_kbps: None,
                link_kbps: None,
                candidates: Vec::new(),
                input_key: false,
                input: Default::default(),
                gamepad: None,
            },
        );
        self.ctx.core.emit(SessionEvent::GuestJoined {
            guest,
            user,
            name,
            fingerprint,
            safety_number,
            permissions,
        });
        if self.ctx.config.force_relay {
            let ctx = Arc::clone(&self.ctx);
            tokio::spawn(async move { ensure_relay(&ctx).await });
        }
        Ok(())
    }

    fn on_mls(&mut self, guest: GuestId, payload: &[u8]) {
        let Some(member) = self.members.get(&guest) else {
            return;
        };
        let Some(Payload::PairwiseOffer(offer)) = Payload::decode(payload) else {
            debug!(guest = guest.0, "mensaje MLS de un invitado ignorado");
            return;
        };
        let accepted = PairwiseOffer::from_bytes(offer).and_then(|offer| {
            require_pq_offer(&offer)?;
            hpke::accept_pairwise_secret(
                &self.ctx.core.identity,
                &member.key,
                &offer,
                &pairwise_info(&self.code),
            )
        });
        match accepted {
            Ok(secret) => {
                if let Err(err) = self.ctx.input.add_guest(guest, &secret) {
                    self.ctx.core.warn(format!(
                        "no se pudo preparar la clave de entrada del invitado {}: {err}",
                        guest.0
                    ));
                }
                lock(&self.ctx.core.input_keys).insert(guest.0, secret);
                if let Some(status) = lock(&self.ctx.core.guests).get_mut(&guest) {
                    status.input_key = true;
                }
                self.ctx
                    .core
                    .emit(SessionEvent::InputKeyReady { peer: guest.0 });
            }
            Err(err) => self.ctx.core.warn(format!(
                "la oferta de clave de entrada del invitado {} no es válida: {err}",
                guest.0
            )),
        }
    }

    /// Saca a un invitado de la sala: época nueva al momento, commit a todos (también al
    /// expulsado, para que sepa que está fuera) y se cierra su enlace.
    async fn remove(&mut self, guest: GuestId, kicked: bool) -> Result<(), SessionError> {
        let Some(member) = self.members.remove(&guest) else {
            return Ok(());
        };
        match self.group.remove_member(member.leaf) {
            Ok(change) => {
                let commit = Payload::Commit(&change.commit).encode();
                let mut recipients: Vec<GuestId> = self.members.keys().copied().collect();
                if kicked {
                    recipients.push(guest);
                }
                lock(&self.ctx.pending).take();
                let keys = self.group.media_keys().map_err(crypto("MLS"))?;
                self.ctx.install(change.epoch, &keys);
                // El que sale conoce las claves del chat anteriores: se olvidan.
                self.install_chat_key(true);
                for other in recipients {
                    self.mls(other, commit.clone()).await?;
                }
            }
            Err(err) => self.ctx.core.warn(format!(
                "no se pudo sacar al invitado {} del grupo: {err}",
                guest.0
            )),
        }
        if kicked {
            send(&self.ctx.signal, SignalMessage::Kick { peer_id: guest.0 }).await?;
        }
        let closed = {
            let mut links = lock(&self.ctx.links);
            let direct = links.direct.remove(&guest);
            if let Some(relay) = &mut links.relay
                && relay.guests.remove(&guest).is_some()
            {
                relay.sender.forget_source(u64::from(guest.0));
            }
            direct
        };
        if let Some(link) = closed {
            let _ = link.control.send(&ControlMessage::Bye);
            link.sender.connection().close(if kicked {
                "expulsado"
            } else {
                "fuera de la sala"
            });
        }
        self.ctx.endpoint.revoke(&member.key);
        self.connecting.remove(&guest);
        // Su entrada se suelta y su ranura queda libre antes de olvidarle.
        self.ctx.input.remove_guest(guest);
        self.ctx.voice_state(guest, false);
        lock(&self.ctx.core.guests).remove(&guest);
        lock(&self.ctx.core.input_keys).remove(&guest.0);
        // Lo que escribió se queda en el historial; su límite de ritmo, no.
        lock(&self.ctx.core.chat).forget_guest(guest.0);
        self.ctx.update_active();
        self.ctx.broadcast_members();
        self.ctx
            .core
            .emit(SessionEvent::GuestLeft { guest, kicked });
        Ok(())
    }

    /// Instala en el chat la clave de la época actual del grupo.
    fn install_chat_key(&self, forget_older: bool) {
        let installed = self
            .group
            .chat_key()
            .map_err(|err| err.to_string())
            .and_then(|key| lock(&self.ctx.core.chat).install(key, forget_older));
        if let Err(err) = installed {
            self.ctx
                .core
                .warn(format!("no se pudo preparar la clave del chat: {err}"));
        }
    }

    async fn shutdown(&self) {
        for control in lock(&self.ctx.links).controls() {
            let _ = control.send(&ControlMessage::Bye);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.ctx.endpoint.close();
    }
}

#[allow(clippy::too_many_arguments)]
async fn host_main(
    core: Arc<Core>,
    config: RemoteConfig,
    video: LoopbackConfig,
    encoders: EncoderSet,
    links: Arc<std::sync::Mutex<Links>>,
    encode: Arc<EncodeControl>,
    gate: Arc<KeyframeGate>,
    input: Arc<HostInput>,
    audio_info: AudioInfo,
    commands: mpsc::UnboundedReceiver<Command>,
) {
    let result = run_host(
        Arc::clone(&core),
        config,
        video,
        encoders,
        links,
        encode,
        gate,
        input,
        audio_info,
        commands,
    )
    .await;
    core.end(result.err());
}

#[allow(clippy::too_many_arguments)]
async fn run_host(
    core: Arc<Core>,
    config: RemoteConfig,
    video: LoopbackConfig,
    encoders: EncoderSet,
    links: Arc<std::sync::Mutex<Links>>,
    encode: Arc<EncodeControl>,
    gate: Arc<KeyframeGate>,
    input: Arc<HostInput>,
    audio_info: AudioInfo,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<(), SessionError> {
    let identity = Arc::clone(&core.identity);
    let endpoint = Endpoint::bind(
        &identity,
        config.bind,
        EndpointOptions {
            congestion: Congestion::Media,
            initial_target_kbps: super::start_kbps(video.bitrate_kbps),
        },
    )
    .map_err(network("no se pudo abrir el socket UDP"))?;
    let mapping_task = super::start_port_mapping(&config, &endpoint);
    let mut mapping_rx = super::watch_port_mapping(&core, mapping_task);
    let mut signal = tokio::time::timeout(
        REPLY_TIMEOUT,
        SignalClient::connect(&config.server_url, &config.token, &endpoint, &config.trust),
    )
    .await
    .map_err(|_| SessionError::Network("el servidor no responde".to_owned()))?
    .map_err(signal_error)?;
    let server = ServerUrl::parse(&config.server_url).map_err(network("URL del servidor"))?;
    signal
        .send(&SignalMessage::CreateRoom {
            identity_pk: identity.public_key(),
            name: config.room_name.clone(),
        })
        .await
        .map_err(network("señalización"))?;

    // El relay llega como `RelayToken` o, si la sala no lo tiene, como `QuotaExceeded` o
    // `RelayDisabled`.
    let (mut code, mut relay) = (None, None);
    while code.is_none() || relay.is_none() {
        let message = tokio::time::timeout(REPLY_TIMEOUT, signal.next_event())
            .await
            .map_err(|_| SessionError::Network("el servidor no creó la sala a tiempo".into()))?
            .map_err(network("señalización"))?;
        match message {
            Some(SignalMessage::RoomCreated { code: created }) => code = Some(created),
            Some(SignalMessage::RelayToken { token, addr }) => relay = Some(Some((token, addr))),
            Some(SignalMessage::QuotaExceeded { message }) => {
                core.emit(SessionEvent::RelayUnavailable(message));
                relay = Some(None);
            }
            Some(SignalMessage::RelayDisabled) => {
                core.emit(SessionEvent::RelayUnavailable(RELAY_DISABLED.to_owned()));
                relay = Some(None);
            }
            Some(SignalMessage::Error { message, .. }) => {
                return Err(SessionError::Rejected(message));
            }
            Some(other) => debug!(?other, "mensaje inesperado antes de crear la sala"),
            None => {
                return Err(SessionError::Network(
                    "el servidor cerró la señalización".into(),
                ));
            }
        }
    }
    let (Some(code), Some(relay)) = (code, relay) else {
        return Err(SessionError::Network("sala incompleta".into()));
    };
    if config.force_relay && relay.is_none() {
        return Err(SessionError::Rejected(
            "se pidió el relay (--force-relay) pero la sala no lo tiene".to_owned(),
        ));
    }

    let group = RoomGroup::create(&identity).map_err(crypto("MLS"))?;
    core.own_leaf
        .store(group.own_leaf_index(), Ordering::Release);
    core.host_leaf
        .store(group.own_leaf_index(), Ordering::Release);
    core.install_epoch(group.epoch(), &group.media_keys().map_err(crypto("MLS"))?);
    lock(&core.chat)
        .install(group.chat_key().map_err(crypto("MLS"))?, true)
        .map_err(SessionError::Crypto)?;
    // Los candidatos locales y el reflejado, ya; la WAN que abra el router se añade cuando
    // conteste (rama del bucle principal) y se vuelve a anunciar a los invitados.
    let observed = signal.observed();
    let candidates = if config.force_relay {
        mapping_rx = None;
        Vec::new()
    } else {
        advertised_candidates(&config, &endpoint, observed, None)
    };
    *lock(&core.room_code) = Some(code.clone());

    let ctx = Arc::new(HostCtx {
        core: Arc::clone(&core),
        relay_info: std::sync::Mutex::new(relay.map(|(token, addr)| RelayInfo {
            addr,
            server_name: server.host,
            trust: config.trust.clone(),
            token,
            role: WireRole::Host,
        })),
        config,
        video: std::sync::Mutex::new(video),
        endpoint,
        signal: signal.sender(),
        links,
        encode,
        gate,
        pending: std::sync::Mutex::new(None),
        relay_lock: tokio::sync::Mutex::new(()),
        hello: Hello {
            protocol_version: PROTOCOL_VERSION,
            identity_pk: identity.public_key(),
            role: WireRole::Host,
            relay_token: None,
        },
        max_kbps: AtomicU32::new(video.bitrate_kbps),
        format_note: std::sync::Mutex::new(None),
        encoders: std::sync::Mutex::new(encoders),
        guest_caps: std::sync::Mutex::new(BTreeMap::new()),
        current: tokio::sync::Mutex::new(None),
        input,
        audio_info,
        announced_sharing: AtomicBool::new(true),
    });
    // Cuando el flujo cambia de tamaño (otra captura, o el tope que la reduce), los invitados se
    // enteran por `HostFrameSize`, además de por el propio flujo.
    ctx.encode.set_size_listener(Box::new({
        let ctx = Arc::downgrade(&ctx);
        move |size| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.announce_frame_size(size);
            }
        }
    }));
    let mut room = Room {
        ctx: Arc::clone(&ctx),
        group,
        code: code.clone(),
        candidates,
        members: BTreeMap::new(),
        connecting: BTreeMap::new(),
    };
    core.emit(SessionEvent::RoomCreated {
        code,
        fingerprint: identity.fingerprint(),
    });
    core.set_state(SessionState::Active);
    tokio::spawn(rate_loop(Arc::clone(&ctx)));
    tokio::spawn(cursor_loop(Arc::clone(&ctx)));
    if ctx.config.force_relay {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move { ensure_relay(&ctx).await });
    }

    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let result = loop {
        tokio::select! {
            report = super::mapping_report(&mut mapping_rx) => {
                mapping_rx = None;
                if let Some(report) = report
                    && let Err(err) = room.announce_mapping(&report, observed).await
                {
                    break Err(err);
                }
            }
            message = signal.next_event() => match message {
                Ok(Some(message)) => {
                    if let Err(err) = room.on_signal(message).await {
                        break Err(err);
                    }
                }
                Ok(None) => break Err(SessionError::Network(
                    "el servidor cerró la señalización".into(),
                )),
                Err(err) => break Err(network("señalización")(err)),
            },
            command = commands.recv() => match command {
                Some(Command::Kick(guest)) => {
                    if let Err(err) = room.remove(guest, true).await {
                        break Err(err);
                    }
                }
                Some(Command::PermissionsChanged(guest)) => ctx.send_permissions(guest),
                Some(Command::GamepadMode { guest, slot, raw }) => {
                    ctx.send_to_guest(guest, &ControlMessage::GamepadMode { slot, raw });
                }
                Some(Command::GamepadOutput { guest, slot, data }) => {
                    ctx.send_to_guest(guest, &ControlMessage::GamepadOutput { slot, data });
                }
                Some(Command::Video(change)) => ctx.change_video(change),
                Some(Command::Chat(text)) => ctx.chat_from_host(text),
                Some(Command::ChatClear) => ctx.chat_clear(),
                Some(Command::OfferHandoff(guest)) => {
                    ctx.send_to_guest(guest, &ControlMessage::HandoffOffer);
                }
                Some(Command::MoveToRoom { code, host_name, except }) => {
                    let message = ControlMessage::MoveToRoom { code, host_name };
                    for (guest, control) in lock(&ctx.links).guest_controls() {
                        if guest != except {
                            let _ = control.send(&message);
                        }
                    }
                }
                Some(Command::Reconnect) => {}
                Some(Command::Invite(user)) => {
                    let invite = SignalMessage::Invite { to_user: user, code: room.code.clone() };
                    if let Err(err) = send(&ctx.signal, invite).await {
                        break Err(err);
                    }
                }
                Some(Command::DeclineJoin(user)) => {
                    let decline = SignalMessage::JoinDecline { to_user: user };
                    if let Err(err) = send(&ctx.signal, decline).await {
                        break Err(err);
                    }
                }
                Some(Command::Close) | None => break Ok(()),
            },
            _ = tick.tick() => {
                if core.shared.stopping() {
                    break Ok(());
                }
                ctx.announce_sharing();
            }
        }
    };
    room.shutdown().await;
    signal.close();
    result
}

/// Error de la conexión de señalización, con el texto del servidor si lo rechazó.
pub(crate) fn signal_error(err: coroom_transport::TransportError) -> SessionError {
    match err {
        coroom_transport::TransportError::Rejected { message, .. } => {
            SessionError::Rejected(message)
        }
        other => SessionError::Network(format!("no se pudo conectar con el servidor: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTT: Duration = Duration::from_millis(20);
    const INTERVAL: Duration = Duration::from_micros(16_667);

    /// Las peticiones de una tormenta (cada grupo perdido pide un intra, de varios invitados) se
    /// agrupan: uno ya, los demás esperan a que llegue y, si siguen, salen espaciados cada vez más.
    #[test]
    fn keyframe_requests_are_coalesced_and_spaced() {
        let gate = KeyframeGate::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        assert_eq!(
            gate.on_request(10, RTT, INTERVAL, at(0)),
            KeyframeDecision::Now
        );
        // Mientras no ha salido, todo lo demás se agrupa con él.
        for group in 11..20 {
            assert_eq!(
                gate.on_request(group, RTT, INTERVAL, at(5)),
                KeyframeDecision::Ignore
            );
        }
        // Sale en el grupo 20 a los 10 ms: lo pedido por grupos anteriores ya está servido.
        gate.on_sent(20, at(10));
        assert_eq!(
            gate.on_request(18, RTT, INTERVAL, at(30)),
            KeyframeDecision::Ignore
        );
        // Una pérdida posterior al intra antes de RTT + 2 intervalos + margen (63 ms): se aplaza
        // al final de esa espera y se agrupa con las siguientes.
        let decision = gate.on_request(21, RTT, INTERVAL, at(40));
        let KeyframeDecision::Later(wait) = decision else {
            unreachable!("se aplaza: {decision:?}");
        };
        assert!(
            wait > Duration::ZERO && wait < Duration::from_millis(200),
            "{wait:?}"
        );
        assert_eq!(
            gate.on_request(22, RTT, INTERVAL, at(41)),
            KeyframeDecision::Ignore
        );
        gate.fire_deferred(at(40) + wait);
        let sent = at(40) + wait + Duration::from_millis(5);
        gate.on_sent(30, sent);
        // Otro seguido: la espera se duplica (retroceso).
        let decision = gate.on_request(31, RTT, INTERVAL, sent + Duration::from_millis(70));
        assert!(
            matches!(decision, KeyframeDecision::Later(_)),
            "{decision:?}"
        );
        // Pasado un segundo sin intra forzados, vuelve la espera corta.
        gate.fire_deferred(at(400));
        gate.on_sent(40, at(410));
        assert_eq!(
            gate.on_request(41, RTT, INTERVAL, at(2_000)),
            KeyframeDecision::Now
        );
    }

    /// Un intra pedido que nunca sale (el codificador aún no estaba) no bloquea las peticiones.
    #[test]
    fn a_keyframe_that_never_left_does_not_block_later_requests() {
        let gate = KeyframeGate::default();
        let start = Instant::now();
        assert_eq!(
            gate.on_request(0, RTT, INTERVAL, start),
            KeyframeDecision::Now
        );
        assert_eq!(
            gate.on_request(0, RTT, INTERVAL, start + Duration::from_millis(100)),
            KeyframeDecision::Ignore
        );
        assert_eq!(
            gate.on_request(0, RTT, INTERVAL, start + MAX_KEYFRAME_SPACING),
            KeyframeDecision::Now
        );
    }
}
