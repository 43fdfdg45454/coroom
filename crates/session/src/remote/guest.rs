//! Invitado de la sesión remota: entra en la sala, se une al grupo MLS, conecta y presenta;
//! también oye el juego y las voces y habla si se le permite.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use coroom_audio::processing::VoiceProcessor;
use coroom_codec::{DecoderSet, decoder_label};
use coroom_transport::PeerConnection;
use coroom_transport::connect::{Connector, RelayInfo};
use coroom_transport::crypto::hpke;
use coroom_transport::crypto::{
    KeyPackageBundle, Processed, RoomGroup, SafetyNumber, sha256_fingerprint,
};
use coroom_transport::media::{MediaReceiver, MediaReceiverConfig, audio_datagram};
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_transport::proto::control::{ControlMessage, Hello, Role as WireRole};
use coroom_transport::proto::media::{ObjectHeader, Track};
use coroom_transport::proto::signal::{ErrorCode, HOST_PEER_ID, SignalMessage};
use coroom_transport::quic::{ControlReceiver, ControlSender};
use coroom_transport::signal::{ServerUrl, SignalClient, SignalSender};
use coroom_transport::{Endpoint, EndpointOptions, PublicKey};
use crossbeam_channel::{Receiver, Sender, bounded};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tracing::debug;

use super::audio::{
    AudioOut, GuestAudio, Packet, Player, TalkMode, extend_us, packet_channel, play_loop,
    voice_capture_loop,
};
use super::host::signal_error;
use super::negotiate::{GuestCapabilities, codec_from_wire, format_from_wire, shape_from_message};
use super::wire::{Payload, host_recipient, pairwise_info};
use super::{
    Command, Core, GUEST_FRAME_INTERVAL, MAX_RECONNECT_ATTEMPTS, QUALITY_PING_PERIOD, QualityLevel,
    REPLY_TIMEOUT, RemoteConfig, SessionEvent, Started, advertised_candidates, lock,
    reconnect_delay,
};
use crate::latency::{AudioTrack, Stage};
use crate::loopback::{
    CHANNEL_CAPACITY, Compressed, DecodeControl, FrameSink, decode_loop, extend_frame_id,
    present_loop, send_latest, spawn_stage,
};
use crate::{SessionError, SessionState};

/// Espera a la explicación de la señalización cuando se cae la conexión con el anfitrión.
const DISCONNECT_GRACE: Duration = Duration::from_secs(1);

/// Intervalo mínimo entre dos `KeyframeRequest` del invitado mientras no llegue un intra: evita
/// pedir otro cuando el anterior aún viene de camino. En cuanto llega un intra se puede volver a
/// pedir sin esperar (si se pierde el fotograma siguiente, no hay que congelar la imagen 250 ms).
const KEYFRAME_REQUEST_PERIOD: Duration = Duration::from_millis(250);

/// Fotogramas comprimidos que esperan al decodificador como mucho. Todo fotograma de vídeo es
/// referencia: descartar uno obliga a pedir un intra y congela la imagen un RTT o más. Tras esperar
/// un reenvío (hasta 400 ms, `coroom_transport::media`), el receptor entrega de golpe los que
/// venían detrás (12 a 30 fps); con la capacidad de 2 de las demás etapas se descartaban casi todos
/// y cada `Nack` acababa en intra (lo destapó la matriz de red). Con el decodificador
/// al día la cola está vacía y no añade latencia; los fotogramas decodificados sí se descartan
/// (solo se presenta el último).
const DECODE_QUEUE_CAPACITY: usize = 16;

/// Marcas del anfitrión que esperan a que haya desfase de reloj, como mucho.
const MAX_PENDING_TIMINGS: usize = 512;

/// Canal hacia el hilo `decode`: el emisor y un receptor para sacar el más antiguo si está lleno.
type FrameQueue = (Sender<Compressed>, Receiver<Compressed>);

/// Arranca los hilos del invitado y su tarea de red.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start(
    core: &Arc<Core>,
    config: RemoteConfig,
    code: String,
    decoders: DecoderSet,
    sink: Box<dyn FrameSink>,
    frames: Option<u64>,
    guest_audio: Option<GuestAudio>,
    handle: &Handle,
    commands: mpsc::UnboundedReceiver<Command>,
) -> Result<Started, SessionError> {
    if decoders.is_empty() {
        return Err(SessionError::Media(
            "no hay ningún decodificador disponible con las opciones dadas".into(),
        ));
    }
    let decode =
        Arc::new(DecodeControl::new(decoders).accepting(config.accept_yuv444, config.accept_hdr));
    let (rcv_tx, rcv_rx) = bounded::<Compressed>(DECODE_QUEUE_CAPACITY);
    let (dec_tx, dec_rx) = bounded(CHANNEL_CAPACITY);
    let spawn_error = |err: crate::loopback::PipelineError| SessionError::Media(err.to_string());
    let mut threads = Vec::with_capacity(2);
    {
        let (shared, ledger, evict, rcv_rx, decode) = (
            Arc::clone(&core.shared),
            Arc::clone(&core.ledger),
            dec_rx.clone(),
            rcv_rx.clone(),
            Arc::clone(&decode),
        );
        let handle = spawn_stage(&core.shared, "decode", move || {
            decode_loop(None, rcv_rx, dec_tx, evict, &shared, &ledger, Some(&decode))
        })
        .map_err(spawn_error)?;
        threads.push(("decode", handle));
    }
    {
        let (shared, ledger) = (Arc::clone(&core.shared), Arc::clone(&core.ledger));
        let handle = spawn_stage(&core.shared, "present", move || {
            present_loop(sink, dec_rx, frames, &shared, &ledger)
        })
        .map_err(spawn_error)?;
        threads.push(("present", handle));
    }
    if let Some(guest_audio) = guest_audio {
        start_audio(core, guest_audio, &mut threads)?;
    }
    let evict = rcv_rx.clone();
    let main = handle.spawn(guest_main(
        Arc::clone(core),
        config,
        code,
        (rcv_tx, evict),
        decode,
        commands,
    ));
    Ok(Started { main, threads })
}

/// Arranca los hilos de audio del invitado (`audio-play` y, con voz, `voice-cap`).
fn start_audio(
    core: &Arc<Core>,
    guest_audio: GuestAudio,
    threads: &mut Vec<(&'static str, std::thread::JoinHandle<()>)>,
) -> Result<(), SessionError> {
    let spawn_error = |err: crate::loopback::PipelineError| SessionError::Media(err.to_string());
    let control = core.audio.clone();
    control.set_game_muted(guest_audio.mute_game);
    let frame_ms = if guest_audio.frame_ms >= 20 { 20 } else { 10 };
    let processor =
        guest_audio
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
    if let Some(speaker) = guest_audio.speaker {
        let (tx, rx) = packet_channel();
        *lock(&core.audio_tx) = Some(tx);
        let (thread_core, control) = (Arc::clone(core), control.clone());
        let handle = spawn_stage(&core.shared, "audio-play", move || {
            let clock = Arc::clone(&thread_core);
            let to_local = move |capture_us: u32| {
                let receiver = lock(&clock.link).receiver.clone()?;
                let now = receiver.remote_us(Instant::now())?;
                receiver.remote_instant(extend_us(now, capture_us))
            };
            let player = Player {
                sink: speaker,
                control,
                render,
                av_sync: true,
            };
            play_loop(&thread_core, player, &rx, &to_local);
            Ok(())
        })
        .map_err(spawn_error)?;
        threads.push(("audio-play", handle));
    }
    if let (Some(voice), Some(processor)) = (guest_audio.voice, processor) {
        control.set_voice_enabled(true);
        control.set_push_to_talk(voice.talk == TalkMode::PushToTalk);
        let out = GuestAudioOut {
            core: Arc::clone(core),
        };
        let thread_core = Arc::clone(core);
        let handle = spawn_stage(&core.shared, "voice-cap", move || {
            voice_capture_loop(&thread_core, voice, processor, frame_ms, &control, &out);
            Ok(())
        })
        .map_err(spawn_error)?;
        threads.push(("voice-cap", handle));
    }
    Ok(())
}

/// La voz del invitado va al anfitrión por su conexión (directa o por el relay).
struct GuestAudioOut {
    core: Arc<Core>,
}

impl AudioOut for GuestAudioOut {
    fn send(&self, track: Track, seq: u32, sealed: &[u8]) -> bool {
        let Some(connection) = lock(&self.core.link).connection.clone() else {
            return false;
        };
        let epoch = self.core.epoch.load(Ordering::Acquire) as u16;
        audio_datagram(track, seq, epoch, sealed)
            .is_some_and(|datagram| connection.send_datagram(datagram).is_ok())
    }

    fn has_targets(&self, _track: Track) -> bool {
        lock(&self.core.link).connection.is_some()
    }

    fn host_us(&self, at: Instant) -> Option<u64> {
        lock(&self.core.link).receiver.clone()?.remote_us(at)
    }

    fn speaking(&self, speaking: bool) {
        if let Some(control) = lock(&self.core.link).control.clone() {
            let _ = control.send(&ControlMessage::VoiceState { guest: 0, speaking });
        }
    }
}

/// Recibe el audio del juego (solo del anfitrión) y las voces (de cualquiera menos de uno mismo)
/// y los pasa a `audio-play`.
async fn audio_receiver(core: Arc<Core>, connection: PeerConnection, playing: Arc<AtomicBool>) {
    let mut decryptor = core.ring.decryptor();
    let mut plain = Vec::with_capacity(2048);
    while let Ok(datagram) = connection.read_audio_datagram().await {
        // Durante la estimación de calidad se descarta (sin acumular: el audio arranca con el
        // vídeo, y la sincronía A/V no tiene que recortar nada).
        if !playing.load(Ordering::Acquire) {
            continue;
        }
        let arrived = Instant::now();
        let Ok((header, payload)) = ObjectHeader::parse_datagram(&datagram) else {
            continue;
        };
        let track = match header.track {
            Track::Audio => AudioTrack::Game,
            Track::Voice => AudioTrack::Voice,
            _ => continue,
        };
        let source = if core.sframe {
            match decryptor.decrypt_into(payload, &mut plain) {
                Ok((info, _)) if info.track == header.track as u8 => info.sender_leaf,
                _ => continue,
            }
        } else {
            plain.clear();
            plain.extend_from_slice(payload);
            core.host_leaf.load(Ordering::Acquire)
        };
        let host = core.host_leaf.load(Ordering::Acquire);
        let own = core.own_leaf.load(Ordering::Acquire);
        let accept = match track {
            AudioTrack::Game => source == host,
            // La propia vuelve por el abanico del relay; sin permiso de voz no se oye a nadie.
            AudioTrack::Voice => (source != own || !core.sframe) && core.audio.voice_allowed(),
        };
        if !accept {
            continue;
        }
        if let Some(tx) = lock(&core.audio_tx).as_ref()
            && let Some(packet) = Packet::parse(track, source, &plain, arrived)
        {
            let _ = tx.try_send(packet);
        }
    }
}

/// Cómo terminó una entrada en la sala.
enum Exit {
    /// Se cerró la sesión.
    Closed,
    /// Se pidió volver a entrar ([`super::RemoteSession::reconnect`]).
    Reconnect,
}

/// Bucle del invitado: entra en la sala y, si la conexión se pierde por la red después de haber
/// entrado, vuelve a entrar hasta [`MAX_RECONNECT_ATTEMPTS`] veces seguidas, con la espera de
/// [`reconnect_delay`] entre intentos. Una migración de QUIC no llega aquí: la conexión
/// sigue. La expulsión, el cierre de la sala o un rechazo del servidor terminan la sesión.
async fn guest_main(
    core: Arc<Core>,
    config: RemoteConfig,
    code: String,
    frames: FrameQueue,
    decode: Arc<DecodeControl>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let result = loop {
        let outcome = run_guest(
            Arc::clone(&core),
            config.clone(),
            code.clone(),
            frames.clone(),
            Arc::clone(&decode),
            &mut commands,
        )
        .await;
        // Al terminar del todo, el enlace se queda como estaba: el resumen final lo enseña.
        let err = match outcome {
            Ok(Exit::Closed) => break Ok(()),
            Ok(Exit::Reconnect) => {
                clear_link(&core);
                core.reconnect_attempts.store(1, Ordering::Release);
                core.emit(SessionEvent::Reconnecting {
                    attempt: 1,
                    of: MAX_RECONNECT_ATTEMPTS,
                    retry_in: Duration::ZERO,
                    reason: "reconexión pedida".to_owned(),
                });
                continue;
            }
            Err(err) => err,
        };
        if !should_reconnect(&core, &err) {
            break Err(err);
        }
        clear_link(&core);
        let attempt = core.reconnect_attempts.fetch_add(1, Ordering::AcqRel) + 1;
        if attempt > MAX_RECONNECT_ATTEMPTS {
            break Err(err);
        }
        let delay = reconnect_delay(attempt);
        core.emit(SessionEvent::Reconnecting {
            attempt,
            of: MAX_RECONNECT_ATTEMPTS,
            retry_in: delay,
            reason: err.to_string(),
        });
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            command = commands.recv() => match command {
                Some(Command::Close) | None => break Ok(()),
                // Ya se está reconectando: el intento va ya.
                Some(_) => {}
            },
        }
        if core.shared.stopping() {
            break Ok(());
        }
    };
    core.end(result.err());
}

/// Solo se reconecta tras perder la red habiendo entrado alguna vez en la sala.
fn should_reconnect(core: &Core, err: &SessionError) -> bool {
    matches!(err, SessionError::Network(_))
        && core.joined_once.load(Ordering::Acquire)
        && !core.removed.load(Ordering::Acquire)
        && !core.shared.stopping()
}

/// Olvida la conexión con el anfitrión (para la interfaz: «sin conectar»).
fn clear_link(core: &Core) {
    let mut link = lock(&core.link);
    link.path = None;
    link.connection = None;
    link.receiver = None;
    link.control = None;
}

fn network(what: &str) -> impl Fn(coroom_transport::TransportError) -> SessionError + '_ {
    move |err| SessionError::Network(format!("{what}: {err}"))
}

/// Estado de la sala del invitado (solo lo toca la tarea principal).
struct Room {
    core: Arc<Core>,
    config: RemoteConfig,
    code: String,
    endpoint: Endpoint,
    signal: SignalSender,
    server_name: String,
    host_pk: PublicKey,
    bundle: Option<KeyPackageBundle>,
    group: Option<RoomGroup>,
    relay: Option<RelayInfo>,
    /// Si ya se sabe si hay relay (`RelayToken`, o `QuotaExceeded`/`RelayDisabled` si no).
    relay_known: bool,
    candidates: Option<Vec<SocketAddr>>,
    connection: Option<tokio::task::JoinHandle<()>>,
    frames: FrameQueue,
    decode: Arc<DecodeControl>,
    disconnected: mpsc::UnboundedSender<Result<(), SessionError>>,
}

impl Room {
    async fn on_signal(&mut self, message: SignalMessage) -> Result<(), SessionError> {
        match message {
            SignalMessage::Mls {
                from_peer: HOST_PEER_ID,
                payload,
                ..
            } => self.on_mls(&payload).await,
            SignalMessage::Candidates {
                from_peer: HOST_PEER_ID,
                addrs,
                ..
            } => {
                self.candidates = Some(addrs);
                self.maybe_connect();
                Ok(())
            }
            SignalMessage::RelayToken { token, addr } => {
                self.relay = Some(RelayInfo {
                    addr,
                    server_name: self.server_name.clone(),
                    trust: self.config.trust.clone(),
                    token,
                    role: WireRole::Guest,
                });
                self.relay_known = true;
                self.maybe_connect();
                Ok(())
            }
            SignalMessage::QuotaExceeded { message } => {
                self.no_relay(message);
                Ok(())
            }
            SignalMessage::RelayDisabled => {
                self.no_relay(super::host::RELAY_DISABLED.to_owned());
                Ok(())
            }
            SignalMessage::Invited {
                from_user,
                from_name,
                code,
            } => {
                self.core.emit(SessionEvent::Invited {
                    from_user,
                    from_name,
                    code,
                });
                Ok(())
            }
            SignalMessage::Error { code, message } => match code {
                ErrorCode::Kicked => Err(SessionError::Removed),
                ErrorCode::RoomClosed => Err(SessionError::RoomClosed),
                _ => {
                    self.core.warn(format!("el servidor dice: {message}"));
                    Ok(())
                }
            },
            other => {
                debug!(?other, "mensaje de señalización ignorado");
                Ok(())
            }
        }
    }

    async fn on_mls(&mut self, payload: &[u8]) -> Result<(), SessionError> {
        match Payload::decode(payload) {
            Some(Payload::Welcome(welcome)) => {
                let Some(bundle) = self.bundle.take() else {
                    debug!("Welcome repetido; se ignora");
                    return Ok(());
                };
                let group = RoomGroup::join(welcome, bundle).map_err(|err| {
                    SessionError::Crypto(format!("no se pudo entrar en el grupo: {err}"))
                })?;
                let host_leaf = group.host_leaf_index();
                let host_key = group
                    .members()
                    .into_iter()
                    .find(|member| member.leaf_index == host_leaf)
                    .map(|member| member.public_key);
                if host_key != Some(self.host_pk) {
                    return Err(SessionError::Crypto(
                        "el grupo MLS no lo creó la identidad del anfitrión".to_owned(),
                    ));
                }
                self.core
                    .own_leaf
                    .store(group.own_leaf_index(), Ordering::Release);
                self.core.host_leaf.store(host_leaf, Ordering::Release);
                let keys = group
                    .media_keys()
                    .map_err(|err| SessionError::Crypto(err.to_string()))?;
                self.core.install_epoch(group.epoch(), &keys);
                self.group = Some(group);
                Ok(())
            }
            Some(Payload::Commit(commit)) => {
                let Some(group) = &mut self.group else {
                    debug!("commit antes del Welcome; se ignora");
                    return Ok(());
                };
                match group.process(commit) {
                    Ok(Processed::Commit { epoch, .. }) => {
                        let keys = group
                            .media_keys()
                            .map_err(|err| SessionError::Crypto(err.to_string()))?;
                        self.core.install_epoch(epoch, &keys);
                        Ok(())
                    }
                    Ok(Processed::Removed { .. }) => {
                        // Fuera del grupo: no hay claves nuevas y no se presenta nada más.
                        self.core.removed.store(true, Ordering::Release);
                        Err(SessionError::Removed)
                    }
                    Ok(_) => Ok(()),
                    Err(err) => {
                        self.core
                            .warn(format!("commit MLS del anfitrión rechazado: {err}"));
                        Ok(())
                    }
                }
            }
            Some(Payload::HostPqKey(key)) => {
                self.start_pairwise(key).await;
                Ok(())
            }
            _ => {
                debug!("mensaje MLS desconocido; se ignora");
                Ok(())
            }
        }
    }

    /// El invitado inicia el secreto pareja a pareja con la clave X-Wing firmada del anfitrión.
    async fn start_pairwise(&mut self, signed_key: &[u8]) {
        // Sin respaldo a X25519: el anfitrión siempre publica su clave X-Wing firmada, así que una
        // clave que no verifica es una manipulación en el camino (el servidor la reenvía) y caer a
        // X25519 sería una degradación de la protección poscuántica (`docs/seguridad.md`,
        // hallazgo C-1). La entrada queda desactivada y se avisa.
        let recipient = match host_recipient(self.host_pk, signed_key) {
            Ok(recipient) => recipient,
            Err(err) => {
                self.core.warn(format!(
                    "la clave X-Wing del anfitrión no verifica ({err}): posible manipulación en el \
                     servidor; la entrada queda desactivada en esta sala"
                ));
                return;
            }
        };
        match hpke::derive_pairwise_secret(
            &self.core.identity,
            &recipient,
            &pairwise_info(&self.code),
        ) {
            Ok((offer, secret)) => {
                let payload = Payload::PairwiseOffer(&offer.to_bytes()).encode();
                let sent = self
                    .signal
                    .send(&SignalMessage::Mls {
                        from_peer: 0,
                        to_peer: Some(HOST_PEER_ID),
                        payload,
                    })
                    .await;
                if sent.is_ok() {
                    lock(&self.core.input_keys).insert(HOST_PEER_ID, secret);
                    self.core
                        .emit(SessionEvent::InputKeyReady { peer: HOST_PEER_ID });
                }
            }
            Err(err) => self
                .core
                .warn(format!("no se pudo derivar la clave de entrada: {err}")),
        }
    }

    /// La sala no tiene relay: solo directo.
    fn no_relay(&mut self, message: String) {
        self.relay = None;
        self.relay_known = true;
        self.core.emit(SessionEvent::RelayUnavailable(message));
        self.maybe_connect();
    }

    /// Conecta en cuanto se tienen los candidatos del anfitrión y se sabe si hay relay.
    fn maybe_connect(&mut self) {
        if self.connection.is_some() || !self.relay_known {
            return;
        }
        let (Some(candidates), relay) = (&self.candidates, &self.relay) else {
            return;
        };
        // Sin candidatos del anfitrión (fuerza el relay) no se intenta la vía directa, salvo que
        // no haya relay.
        let force_relay = relay.is_some() && (self.config.force_relay || candidates.is_empty());
        let task = tokio::spawn(connect_and_receive(
            Arc::clone(&self.core),
            self.endpoint.clone(),
            self.host_pk,
            candidates.clone(),
            relay.clone(),
            force_relay,
            (self.config.punch_timeout, self.config.quality_probe),
            self.frames.clone(),
            Arc::clone(&self.decode),
            self.disconnected.clone(),
        ));
        self.connection = Some(task);
    }
}

async fn run_guest(
    core: Arc<Core>,
    config: RemoteConfig,
    code: String,
    frames: FrameQueue,
    decode: Arc<DecodeControl>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> Result<Exit, SessionError> {
    let identity = Arc::clone(&core.identity);
    let endpoint = Endpoint::bind(&identity, config.bind, EndpointOptions::default())
        .map_err(network("no se pudo abrir el socket UDP"))?;
    let mut signal = tokio::time::timeout(
        REPLY_TIMEOUT,
        SignalClient::connect(&config.server_url, &config.token, &endpoint, &config.trust),
    )
    .await
    .map_err(|_| SessionError::Network("el servidor no responde".to_owned()))?
    .map_err(signal_error)?;
    let server = ServerUrl::parse(&config.server_url).map_err(network("URL del servidor"))?;
    let (key_package, bundle) = KeyPackageBundle::generate(&identity)
        .map_err(|err| SessionError::Crypto(err.to_string()))?;
    signal
        .send(&SignalMessage::JoinRoom {
            code: code.clone(),
            identity_pk: identity.public_key(),
            key_package,
        })
        .await
        .map_err(network("señalización"))?;

    let mut backlog = VecDeque::new();
    let (host_pk, host_name) = loop {
        let message = tokio::time::timeout(REPLY_TIMEOUT, signal.next_event())
            .await
            .map_err(|_| SessionError::Network("el servidor no respondió a tiempo".into()))?
            .map_err(network("señalización"))?;
        match message {
            Some(SignalMessage::Joined {
                host_pk, host_name, ..
            }) => break (host_pk, host_name),
            Some(SignalMessage::Error { message, .. }) => {
                return Err(SessionError::Rejected(message));
            }
            Some(other) => backlog.push_back(other),
            None => {
                return Err(SessionError::Network(
                    "el servidor cerró la señalización".into(),
                ));
            }
        }
    };
    let own = identity.public_key();
    let safety_number = SafetyNumber::between(&own, &host_pk);
    let host_fingerprint = sha256_fingerprint(&host_pk);
    {
        let mut link = lock(&core.link);
        link.safety_number = Some(safety_number);
        link.host_fingerprint = host_fingerprint.clone();
        link.host_name = host_name.clone();
    }
    core.joined_once.store(true, Ordering::Release);
    core.emit(SessionEvent::Joined {
        host_name,
        host_fingerprint,
        safety_number,
    });

    // Se admite al anfitrión antes de anunciar candidatos: si llamase antes de que el `Connector`
    // del invitado arranque, su handshake se rechazaría después de que él lo diese por bueno (en
    // TLS 1.3 el cliente termina antes de que el servidor valide su certificado).
    endpoint.admit(host_pk);
    let candidates = if config.force_relay {
        Vec::new()
    } else {
        advertised_candidates(&config, &endpoint, signal.observed())
    };
    signal
        .send(&SignalMessage::Candidates {
            from_peer: 0,
            to_peer: HOST_PEER_ID,
            addrs: candidates,
        })
        .await
        .map_err(network("señalización"))?;

    let (disconnected_tx, mut disconnected) = mpsc::unbounded_channel();
    let mut room = Room {
        core: Arc::clone(&core),
        config,
        code,
        endpoint: endpoint.clone(),
        signal: signal.sender(),
        server_name: server.host,
        host_pk,
        bundle: Some(bundle),
        group: None,
        relay: None,
        relay_known: false,
        candidates: None,
        connection: None,
        frames,
        decode,
        disconnected: disconnected_tx,
    };
    let mut result = Ok(Exit::Closed);
    while let Some(message) = backlog.pop_front() {
        if let Err(err) = room.on_signal(message).await {
            result = Err(err);
            break;
        }
    }

    let mut tick = tokio::time::interval(Duration::from_millis(100));
    // Si la conexión con el anfitrión se cae, se espera un poco a que la señalización diga por qué
    // (expulsión o sala cerrada): el cierre de QUIC llega antes que el aviso del servidor.
    let mut lost: Option<(SessionError, Instant)> = None;
    if result.is_ok() {
        result = loop {
            tokio::select! {
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
                    Some(Command::Close) | None => break Ok(Exit::Closed),
                    Some(Command::Reconnect) => break Ok(Exit::Reconnect),
                    Some(Command::Kick(_) | Command::PermissionsChanged(_) | Command::Invite(_)) => {}
                },
                Some(end) = disconnected.recv() => match end {
                    Ok(()) => break Ok(Exit::Closed),
                    Err(err) => lost = Some((err, Instant::now() + DISCONNECT_GRACE)),
                },
                _ = tick.tick() => {
                    if core.shared.stopping() && lost.is_none() {
                        break Ok(Exit::Closed);
                    }
                    if let Some((err, deadline)) = &lost
                        && Instant::now() >= *deadline
                    {
                        break Err(err.clone());
                    }
                }
            }
        };
    }
    if let Some(task) = room.connection.take() {
        task.abort();
    }
    endpoint.close();
    signal.close();
    result
}

/// Conecta con el anfitrión (directo o por el relay), abre el control y recibe hasta que se
/// cierre la conexión; avisa a la tarea principal de cómo terminó.
#[allow(clippy::too_many_arguments)]
async fn connect_and_receive(
    core: Arc<Core>,
    endpoint: Endpoint,
    host_pk: PublicKey,
    candidates: Vec<SocketAddr>,
    relay: Option<RelayInfo>,
    force_relay: bool,
    timeouts: (Duration, Duration),
    frames: FrameQueue,
    decode: Arc<DecodeControl>,
    done: mpsc::UnboundedSender<Result<(), SessionError>>,
) {
    let result = receive(
        &core,
        endpoint,
        host_pk,
        &candidates,
        relay.as_ref(),
        force_relay,
        timeouts,
        &frames,
        decode,
    )
    .await;
    let _ = done.send(result);
}

#[allow(clippy::too_many_arguments)]
async fn receive(
    core: &Arc<Core>,
    endpoint: Endpoint,
    host_pk: PublicKey,
    candidates: &[SocketAddr],
    relay: Option<&RelayInfo>,
    force_relay: bool,
    (punch_timeout, probe): (Duration, Duration),
    frames: &FrameQueue,
    decode: Arc<DecodeControl>,
) -> Result<(), SessionError> {
    let identity_pk = endpoint.public_key();
    let established = Connector::new(endpoint)
        .with_timeout(punch_timeout)
        .force_relay(force_relay)
        .connect(host_pk, candidates, relay)
        .await
        .map_err(network("no se pudo conectar con el anfitrión"))?;
    let connection = established.connection;
    let control = tokio::time::timeout(
        REPLY_TIMEOUT,
        connection.open_control(Hello {
            protocol_version: PROTOCOL_VERSION,
            identity_pk,
            role: WireRole::Guest,
            relay_token: None,
        }),
    )
    .await
    .map_err(|_| SessionError::Network("el anfitrión no respondió al control".into()))?
    .map_err(network("control"))?;
    let (reply, control_rx, hello) = control.split();
    if hello.role != WireRole::Host || hello.identity_pk != host_pk {
        connection.close("identidad");
        return Err(SessionError::Crypto(
            "el control no es del anfitrión de la sala".to_owned(),
        ));
    }
    let receiver = Arc::new(MediaReceiver::new(
        connection.clone(),
        reply.clone(),
        MediaReceiverConfig {
            frame_interval: GUEST_FRAME_INTERVAL,
        },
    ));
    {
        let mut link = lock(&core.link);
        link.path = Some(established.path);
        link.connection = Some(connection.clone());
        link.receiver = Some(Arc::clone(&receiver));
        link.control = Some(reply.clone());
    }
    let playing = Arc::new(AtomicBool::new(false));
    let audio_task = lock(&core.audio_tx).is_some().then(|| {
        tokio::spawn(audio_receiver(
            Arc::clone(core),
            connection.clone(),
            Arc::clone(&playing),
        ))
    });
    core.set_state(SessionState::Active);
    core.emit(SessionEvent::Connected {
        path: established.path,
    });
    if core.reconnect_attempts.swap(0, Ordering::AcqRel) > 0 {
        core.emit(SessionEvent::Reconnected {
            path: established.path,
        });
    }

    let (yuv444, hdr) = decode.accept;
    let caps = GuestCapabilities::from_decoders(&decode.set, yuv444, hdr);
    let control_task = tokio::spawn(control_loop(
        Arc::clone(core),
        control_rx,
        Arc::clone(&receiver),
        decode,
    ));
    // Estimación de calidad: RTT con el anfitrión antes de que arranque el códec (el
    // anfitrión no lo crea hasta tener las capacidades de este invitado).
    let rtt_ms = measure_rtt(&receiver, &connection, probe).await;
    core.emit(SessionEvent::QualityEstimate {
        rtt_ms,
        level: QualityLevel::from_rtt_ms(rtt_ms),
    });
    if let Err(err) = reply.send(&caps.to_message()) {
        control_task.abort();
        if let Some(task) = audio_task {
            task.abort();
        }
        connection.close("capacidades");
        return Err(network("capacidades")(err));
    }
    playing.store(true, Ordering::Release);
    // Entrada: stream y emisor cifrados con el secreto pareja a pareja.
    let input_task = {
        let (core, connection, receiver) =
            (Arc::clone(core), connection.clone(), Arc::clone(&receiver));
        tokio::spawn(async move {
            if let Err(err) = super::input::open_guest_input(&core, &connection, &receiver).await {
                core.warn(err);
            }
        })
    };
    let result = media_loop(core, &receiver, &reply, frames).await;
    input_task.abort();
    if let Some(task) = audio_task {
        task.abort();
    }
    lock(&core.link).control = None;
    core.guest_input.set_sender(None);
    control_task.abort();
    let _ = reply.send(&ControlMessage::Bye);
    connection.close("fin");
    result
}

/// RTT con el anfitrión durante `window`: un `Ping` cada [`QUALITY_PING_PERIOD`] y la mediana de
/// sus `Pong` (los atiende `control_loop`); sin `Pong`, el RTT de QUIC.
async fn measure_rtt(
    receiver: &MediaReceiver,
    connection: &PeerConnection,
    window: Duration,
) -> f64 {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        receiver.send_ping();
        tokio::time::sleep(
            QUALITY_PING_PERIOD.min(deadline.saturating_duration_since(Instant::now())),
        )
        .await;
    }
    receiver
        .clock_rtt()
        .unwrap_or_else(|| connection.rtt())
        .as_secs_f64()
        * 1000.0
}

/// Mensajes de control del anfitrión: reloj, marcas de tiempo, bitrate objetivo, códec y cursor.
async fn control_loop(
    core: Arc<Core>,
    mut control: ControlReceiver,
    receiver: Arc<MediaReceiver>,
    decode: Arc<DecodeControl>,
) {
    let mut codec = None;
    let mut timings = Timings::default();
    loop {
        let message = match control.recv().await {
            Ok(Some(message)) => message,
            Ok(None) | Err(_) => break,
        };
        match receiver.handle_control(message) {
            Some(ControlMessage::FrameTiming {
                track: Track::Video,
                group,
                captured_us,
                encoded_us,
                encrypted_us,
                sent_us,
            }) => {
                timings.push(group, [captured_us, encoded_us, encrypted_us, sent_us]);
                timings.flush(&receiver, &core);
            }
            Some(ControlMessage::TargetBitrate { kbps }) => lock(&core.link).target_kbps = kbps,
            Some(ControlMessage::CodecSelected {
                codec: selected,
                fps,
                chroma,
                color,
                ..
            }) => {
                // Los plazos del reensamblado, con los fps reales (acotados por el receptor).
                receiver.set_track_interval(
                    Track::Video,
                    Duration::from_secs_f64(1.0 / f64::from(fps.max(1))),
                );
                let selected = codec_from_wire(selected);
                let format = format_from_wire(chroma, color);
                if codec != Some((selected, format)) {
                    codec = Some((selected, format));
                    decode.request(selected, format);
                    let backend = decode
                        .set
                        .available()
                        .iter()
                        .find(|&&(c, backend)| {
                            c == selected && coroom_codec::decoder_supports(c, backend, format)
                        })
                        .map(|(_, backend)| *backend);
                    let detail = match backend {
                        Some(backend) => {
                            let label = decoder_label(selected, backend);
                            if format.is_default() {
                                format!("decodifica {label}")
                            } else {
                                format!("decodifica {label} · {format}")
                            }
                        }
                        None => format!("sin decodificador para este códec en {format}"),
                    };
                    core.emit(SessionEvent::VideoCodec {
                        codec: selected,
                        detail,
                    });
                }
            }
            Some(ControlMessage::Cursor {
                x,
                y,
                visible,
                shape_id,
            }) => {
                *lock(&core.shared.cursor) = Some(coroom_capture::CursorState {
                    x,
                    y,
                    visible,
                    shape_id,
                });
            }
            Some(ControlMessage::CursorShape {
                shape_id,
                width,
                height,
                hotspot_x,
                hotspot_y,
                rgba,
            }) => {
                match shape_from_message(shape_id, (width, height), (hotspot_x, hotspot_y), rgba) {
                    Some(shape) => {
                        core.shared.remember_shape(shape);
                    }
                    None => debug!(shape_id, "forma de cursor no válida; se ignora"),
                }
            }
            Some(ControlMessage::InputPermissions {
                keyboard_mouse,
                gamepad_slot,
            }) => core
                .guest_input
                .set_allowed(coroom_input::capture::Allowed {
                    keyboard_mouse,
                    gamepad_slot,
                }),
            Some(ControlMessage::HostFrameSize { width, height }) => {
                core.guest_input.set_frame(width, height);
            }
            Some(ControlMessage::InputReport {
                samples,
                p50_us,
                p95_us,
                applied,
                dropped,
            }) => {
                let report = crate::latency::InputLatency {
                    samples: samples as usize,
                    p50_us: u64::from(p50_us),
                    p95_us: u64::from(p95_us),
                    p99_us: u64::from(p95_us),
                    max_us: u64::from(p95_us),
                    applied,
                    dropped,
                };
                core.guest_input.set_report(report);
                core.ledger.set_input_report(report);
            }
            Some(ControlMessage::AudioConfig {
                track,
                sample_rate,
                channels,
                frame_ms,
                bitrate,
            }) => debug!(
                ?track,
                sample_rate, channels, frame_ms, bitrate, "formato de audio del anfitrión"
            ),
            Some(ControlMessage::VoicePermission { allowed }) => {
                core.audio.set_voice_allowed(allowed);
                if !allowed {
                    for who in core.audio.talkers() {
                        core.audio.clear_talker(who);
                    }
                }
            }
            Some(ControlMessage::VoiceState { guest, speaking }) => {
                core.audio.set_talker(guest, speaking);
            }
            Some(ControlMessage::Bye) => break,
            _ => {}
        }
    }
}

/// Marcas del anfitrión a la espera de poder situarlas en el reloj local.
#[derive(Default)]
struct Timings {
    last_id: Option<u64>,
    pending: VecDeque<(u64, [u64; 4])>,
}

impl Timings {
    fn push(&mut self, group: u32, marks: [u64; 4]) {
        let frame_id = extend_frame_id(self.last_id, group);
        self.last_id = Some(frame_id);
        self.pending.push_back((frame_id, marks));
        while self.pending.len() > MAX_PENDING_TIMINGS {
            self.pending.pop_front();
        }
    }

    fn flush(&mut self, receiver: &MediaReceiver, core: &Core) {
        const STAGES: [Stage; 4] = [
            Stage::Captured,
            Stage::Encoded,
            Stage::Encrypted,
            Stage::Sent,
        ];
        if receiver.clock_offset_us().is_none() {
            // Aún no hay desfase: se espera al primer `Pong`.
            return;
        }
        while let Some((frame_id, marks)) = self.pending.pop_front() {
            for (stage, us) in STAGES.iter().zip(marks) {
                if let Some(at) = receiver.remote_instant(us) {
                    core.ledger.mark(frame_id, *stage, at);
                }
            }
        }
    }
}

/// Recibe, descifra y entrega a la decodificación.
async fn media_loop(
    core: &Arc<Core>,
    receiver: &MediaReceiver,
    control: &ControlSender,
    (frames, evict): &FrameQueue,
) -> Result<(), SessionError> {
    let ledger = &core.ledger;
    let shared = &core.shared;
    let mut decryptor = core.ring.decryptor();
    let mut last_id = None;
    let mut dropped_seen = 0u64;
    let mut last_request: Option<Instant> = None;
    loop {
        let frame = receiver.recv_frame().await.map_err(|err| {
            SessionError::Network(format!("se perdió la conexión con el anfitrión: {err}"))
        })?;
        let dropped = receiver.stats().dropped;
        for _ in dropped_seen..dropped {
            ledger.record_drop(Stage::Received);
        }
        dropped_seen = dropped;
        if frame.track != Track::Video {
            continue;
        }
        let frame_id = extend_frame_id(last_id, frame.group);
        last_id = Some(frame_id);
        ledger.mark(frame_id, Stage::Received, frame.received_at);
        if core.removed.load(Ordering::Acquire) || shared.stopping() {
            return Ok(());
        }
        let data = if core.sframe {
            let mut out = Vec::with_capacity(frame.payload.len());
            match decryptor.decrypt_into(&frame.payload, &mut out) {
                Ok((info, _))
                    if info.sender_leaf == core.host_leaf.load(Ordering::Acquire)
                        && info.track == Track::Video as u8 =>
                {
                    out
                }
                Ok(_) => {
                    ledger.record_drop(Stage::Decrypted);
                    continue;
                }
                Err(err) => {
                    debug!(error = %err, group = frame.group, "fotograma sin descifrar");
                    ledger.record_drop(Stage::Decrypted);
                    shared.request_keyframe();
                    request_keyframe(control, frame.group, shared, &mut last_request);
                    continue;
                }
            }
        } else {
            frame.payload
        };
        ledger.mark(frame_id, Stage::Decrypted, Instant::now());
        if frame.keyframe {
            // Llegó un intra: lo pedido ya está servido (también lo que pidió el decodificador por
            // los fotogramas de antes, que el intra sustituye).
            last_request = None;
            let _ = shared.take_keyframe_request();
        }
        // Bitrate recibido (el codificado lo mide el anfitrión).
        ledger.record_encoded_bytes_at(data.len() as u64, frame.received_at);
        let compressed = Compressed {
            frame_id,
            keyframe: frame.keyframe,
            data,
        };
        match send_latest(frames, evict, compressed, ledger, Stage::Decoded) {
            Ok(true) => shared.request_keyframe(),
            Ok(false) => {}
            Err(()) => return Ok(()),
        }
        request_keyframe(control, frame.group, shared, &mut last_request);
    }
}

/// Envía un `KeyframeRequest` si alguien lo pidió (decodificador, descarte o descifrado) y no se
/// pidió otro hace menos de [`KEYFRAME_REQUEST_PERIOD`].
fn request_keyframe(
    control: &ControlSender,
    group: u32,
    shared: &crate::loopback::Shared,
    last: &mut Option<Instant>,
) {
    if last.is_some_and(|at| at.elapsed() < KEYFRAME_REQUEST_PERIOD) {
        return;
    }
    if shared.take_keyframe_request() {
        let _ = control.send(&ControlMessage::KeyframeRequest { group });
        *last = Some(Instant::now());
    }
}
