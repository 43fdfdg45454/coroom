//! Lo que la interfaz no hace en su hilo: la API del servidor, el llavero, las sesiones remotas,
//! la sala de espera (invitaciones y presencia), el diagnóstico y los dispositivos de audio.
//!
//! [`Backend::execute`] recibe cada [`Effect`] de [`super::app::App::update`] y lo hace en un hilo
//! propio (el llavero de Linux y `RemoteSession::start`/`close` bloquean, y no deben ir dentro de
//! un runtime de `tokio`), y devuelve el resultado con `notify` como [`Message::Event`]. Las
//! sesiones mandan sus eventos (un hilo los reenvía) y su estado cada 250-500 ms (otro hilo lo
//! pide), así la ventana nunca espera a la red.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use coroom_render::wgpu::VideoImporter;
use coroom_session::codec::DecodeOutput;
use coroom_session::latency::{LatencyLedger, LatencySummary};
use coroom_session::loopback::LoopbackPipeline;
use coroom_session::preview::PreviewTap;
use coroom_session::remote::{AudioControl, GuestInput, RemoteSession, SessionEvent};
use coroom_session::{Session as _, SessionError};
use coroom_transport::crypto::Identity;
use coroom_transport::proto::signal::SignalMessage;
use coroom_transport::{Endpoint, EndpointOptions, SignalClient};

use super::app::{
    AccountInfo, App, AudioDevice, Effect, Event, HostRequest, Invitation, Message, VideoSupport,
};
use super::screens::guest::Stats;
use super::screens::host::SourceChoice;
use super::settings::{CodecChoice, ServerEntry, Settings, VoiceMode};
use super::video::{FeedSink, VideoFeed, Wake};
use crate::account::{self, AccountOptions, AccountStore};
use crate::probe::{ProbeOptions, Source, start_pipeline_with, to_present_frame};
use crate::remote::{
    AutoAllow, HostAudioOptions, HostOptions, JoinAudioOptions, JoinOptions, NetOptions,
    load_identity, start_guest, start_host,
};

/// Entrega un mensaje a la interfaz (desde cualquier hilo).
pub type Notify = Arc<dyn Fn(Message) + Send + Sync>;

/// Cada cuánto se piden las estadísticas del invitado.
pub const GUEST_STATS_PERIOD: Duration = Duration::from_millis(250);

/// Cada cuánto se pide el estado de los invitados del anfitrión.
pub const HOST_STATUS_PERIOD: Duration = Duration::from_millis(500);

/// Espera antes de reabrir la sala de espera si se cae.
const LOBBY_RETRY: Duration = Duration::from_secs(5);

/// Mandos del invitado que lleva la ventana (teclado, ratón, mandos y audio).
#[derive(Clone)]
pub struct GuestControls {
    /// Entrada hacia el anfitrión.
    pub input: GuestInput,
    /// Audio (silenciar, pulsar para hablar).
    pub audio: AudioControl,
    /// Tecla de pulsar para hablar, si se usa.
    pub push_to_talk: Option<String>,
}

struct GuestRun {
    session: Arc<Mutex<RemoteSession>>,
    controls: GuestControls,
}

struct LocalRun {
    pipeline: Option<LoopbackPipeline>,
    ledger: Arc<LatencyLedger>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ejecutor de efectos.
pub struct Backend {
    notify: Notify,
    wake: Wake,
    importer: Option<VideoImporter>,
    output: DecodeOutput,
    settings_path: PathBuf,
    host: Arc<Mutex<Option<Arc<Mutex<RemoteSession>>>>>,
    guest: Arc<Mutex<Option<GuestRun>>>,
    /// Cancelación de la entrada en curso (antes de que haya sesión que cerrar).
    join_cancel: Option<Arc<AtomicBool>>,
    lobby: Option<Arc<AtomicBool>>,
    local: Arc<Mutex<Option<LocalRun>>>,
    /// Si la ventana puede presentar en HDR.
    hdr_display: Option<bool>,
}

/// Lo que este equipo hace con 4:4:4 y HDR (sondea los códecs la primera vez).
pub fn video_support(hdr_display: Option<bool>) -> VideoSupport {
    use coroom_session::codec::{
        Codec, DecoderSet, EncoderSet, VideoFormat, encoder_label, encoder_supports,
    };
    let encoders = EncoderSet::probe();
    let decoders = DecoderSet::probe();
    let encodes = |format: VideoFormat| -> Vec<(Codec, String)> {
        Codec::PREFERENCE
            .into_iter()
            .filter_map(|codec| {
                let (_, backend) = encoders
                    .available()
                    .iter()
                    .find(|&&(c, b)| c == codec && encoder_supports(c, b, format))?;
                Some((
                    codec,
                    format!("{codec} ({})", encoder_label(codec, *backend)),
                ))
            })
            .collect()
    };
    let decodes = |format: VideoFormat| -> Vec<Codec> {
        Codec::PREFERENCE
            .into_iter()
            .filter(|codec| decoders.supports(*codec, format))
            .collect()
    };
    VideoSupport {
        yuv444: encodes(VideoFormat::SDR_444),
        hdr10: encodes(VideoFormat::HDR10_420),
        decode_yuv444: decodes(VideoFormat::SDR_444),
        decode_hdr10: decodes(VideoFormat::HDR10_420),
        hdr_display,
    }
}

impl Backend {
    /// Ejecutor que avisa con `notify`; `wake` pide dibujar (fotogramas nuevos); el vídeo se
    /// decodifica donde diga `output` y se importa con `importer`.
    pub fn new(
        notify: Notify,
        wake: Wake,
        importer: Option<VideoImporter>,
        output: DecodeOutput,
        settings_path: PathBuf,
    ) -> Self {
        Self {
            notify,
            wake,
            importer,
            output,
            settings_path,
            host: Arc::new(Mutex::new(None)),
            guest: Arc::new(Mutex::new(None)),
            join_cancel: None,
            lobby: None,
            local: Arc::new(Mutex::new(None)),
            hdr_display: None,
        }
    }

    /// Dice si la ventana puede presentar en HDR (para explicarlo en Ajustes).
    pub fn set_hdr_display(&mut self, available: bool) {
        self.hdr_display = Some(available);
    }

    fn spawn(&self, name: &str, job: impl FnOnce(Notify) + Send + 'static) {
        let notify = Arc::clone(&self.notify);
        let spawned = thread::Builder::new()
            .name(format!("coroom-{name}"))
            .spawn(move || job(notify));
        if let Err(err) = spawned {
            (self.notify)(Message::Event(Event::Failed(t!(
                "backend-no-se-pudo-arrancar-una-tarea",
                err = err
            ))));
        }
    }

    /// Mandos del invitado, cuando su sesión ya arrancó.
    pub fn guest_controls(&self) -> Option<GuestControls> {
        lock(&self.guest).as_ref().map(|run| run.controls.clone())
    }

    /// Hace `effect`. `app` da el contexto (cuenta, ajustes).
    pub fn execute(&mut self, effect: Effect, app: &App) {
        let account = app
            .account
            .as_ref()
            .map(|account| account_options(&account.server));
        match effect {
            Effect::Probe { url, tcp_port } => self.spawn("comprobar", move |notify| {
                let result = account::probe_server(&url, tcp_port);
                notify(Message::Event(Event::Probed { url, result }));
            }),
            Effect::SignIn {
                server,
                username,
                password,
                device,
                invitation,
            } => self.spawn("sesion", move |notify| {
                let options = account_options(&server);
                let invitation = invitation
                    .as_ref()
                    .map(|(code, name)| (code.as_str(), name.as_deref()));
                let result = account::sign_in(&options, &username, &password, &device, invitation)
                    .map(|signed| account_info(server, signed));
                notify(Message::Event(Event::SignedIn(result)));
            }),
            Effect::Restore => {
                let servers = app.settings.servers.clone();
                self.spawn("recuperar", move |notify| {
                    notify(Message::Event(restore(&servers)));
                });
            }
            Effect::SignOut => {
                if let Some(options) = account {
                    self.spawn("cerrar", move |notify| {
                        let result = account::logout(&options);
                        notify(Message::Event(Event::SignedOut(result)));
                    });
                }
            }
            Effect::LoadFriends => self.with_session(account, "amigos", |session| {
                Event::Friends(account::list_friends(&session))
            }),
            Effect::AddFriend(name) => self.with_session(account, "amigos", move |session| {
                Event::FriendChanged(account::request_friend(&session, &name))
            }),
            Effect::AcceptFriend(request) => self.with_session(account, "amigos", move |s| {
                Event::FriendChanged(account::accept_request(&s, request))
            }),
            Effect::DeclineFriend(request) => self.with_session(account, "amigos", move |s| {
                Event::FriendChanged(account::decline_request(&s, request))
            }),
            Effect::RemoveFriend(user) => self.with_session(account, "amigos", move |s| {
                Event::FriendChanged(account::remove_friend(&s, user))
            }),
            Effect::LoadDevices => self.with_session(account, "dispositivos", |s| {
                Event::Devices(account::devices(&s))
            }),
            Effect::RevokeDevice(device) => self.with_session(account, "dispositivos", move |s| {
                Event::DeviceRevoked(account::revoke_device(&s, device))
            }),
            Effect::LoadUsage => {
                self.with_session(account, "uso", |s| Event::Usage(account::usage(&s)));
            }
            Effect::StartLobby => {
                if let Some(options) = account {
                    self.start_lobby(options);
                }
            }
            Effect::StopLobby => self.stop_lobby(),
            Effect::StartHost(request) => {
                if let Some(options) = account {
                    self.start_host(options, request, &app.settings);
                }
            }
            Effect::StopHost => {
                let host = lock(&self.host).take();
                if let Some(session) = host {
                    self.spawn("cerrar-sala", move |_| lock(&session).close());
                }
            }
            Effect::SetPermissions(guest, permissions) => {
                if let Some(session) = lock(&self.host).clone()
                    && let Err(err) = lock(&session).set_permissions(guest, permissions)
                {
                    (self.notify)(Message::Event(Event::Failed(err.to_string())));
                }
            }
            Effect::Kick(guest) => {
                if let Some(session) = lock(&self.host).clone()
                    && let Err(err) = lock(&session).kick(guest)
                {
                    (self.notify)(Message::Event(Event::Failed(err.to_string())));
                }
            }
            Effect::PauseInput(paused) => {
                if let Some(session) = lock(&self.host).clone()
                    && let Err(err) = lock(&session).set_paused(paused)
                {
                    (self.notify)(Message::Event(Event::Failed(err.to_string())));
                }
            }
            Effect::Invite(user) => {
                if let Some(session) = lock(&self.host).clone()
                    && let Err(err) = lock(&session).invite(user)
                {
                    (self.notify)(Message::Event(Event::Failed(err.to_string())));
                }
            }
            Effect::Join(code) => {
                if let Some(options) = account {
                    let join = join_options(&options, &app.settings, code);
                    self.start_join(None, join);
                }
            }
            Effect::Leave => {
                let run = lock(&self.guest).take();
                if let Some(run) = run {
                    self.spawn("salir", move |_| lock(&run.session).close());
                } else if let Some(cancel) = self.join_cancel.take() {
                    // Aún conectando: se abandona la entrada y la pantalla se cierra ya.
                    cancel.store(true, Ordering::Relaxed);
                    (self.notify)(Message::Event(Event::GuestEnded(None)));
                }
            }
            Effect::Reconnect => {
                if let Some(run) = lock(&self.guest).as_ref()
                    && let Err(err) = lock(&run.session).reconnect()
                {
                    (self.notify)(Message::Event(Event::Failed(err.to_string())));
                }
            }
            Effect::ToggleMic => {
                if let Some(controls) = self.guest_controls() {
                    controls.audio.toggle_mic();
                }
            }
            Effect::ToggleGameAudio => {
                if let Some(controls) = self.guest_controls() {
                    controls.audio.toggle_game();
                }
            }
            Effect::SaveSettings => {
                if let Err(err) = app.settings.save(&self.settings_path) {
                    (self.notify)(Message::Event(Event::Failed(err)));
                }
            }
            Effect::RunDoctor => self.spawn("diagnostico", |notify| {
                let lines = crate::doctor::report().unwrap_or_else(|err| vec![err]);
                notify(Message::Event(Event::Doctor(lines)));
            }),
            Effect::LoadAudioDevices => self.spawn("audio", |notify| {
                use coroom_session::audio::DeviceKind;
                let result = coroom_session::audio::list_devices()
                    .map(|devices| {
                        devices
                            .into_iter()
                            .filter(|device| device.kind != DeviceKind::Application)
                            .map(|device| AudioDevice {
                                input: device.kind == DeviceKind::Source,
                                name: device.name,
                                description: device.description,
                                default: device.default,
                            })
                            .collect()
                    })
                    .map_err(|err| err.to_string());
                notify(Message::Event(Event::AudioDevices(result)));
            }),
            // Los hace la ventana.
            Effect::CheckUpdates => {
                let version = app.version;
                self.spawn("version", move |notify| {
                    let result = crate::update::check(version);
                    notify(Message::Event(Event::UpdateChecked {
                        result,
                        at: crate::update::unix_now(),
                    }));
                });
            }
            Effect::OpenUrl(url) => self.spawn("enlace", move |notify| {
                if let Err(err) = crate::update::open_url(&url) {
                    notify(Message::Event(Event::Failed(t!(
                        "backend-no-se-pudo-abrir-url-err",
                        url = url,
                        err = err
                    ))));
                }
            }),
            Effect::LoadVideoSupport => {
                let hdr_display = self.hdr_display;
                self.spawn("video", move |notify| {
                    notify(Message::Event(Event::VideoSupport(video_support(
                        hdr_display,
                    ))));
                });
            }
            Effect::Copy(_) | Effect::SetFullscreen(_) | Effect::Quit | Effect::SetImmersive(_) => {
            }
        }
    }

    fn with_session(
        &self,
        account: Option<AccountOptions>,
        name: &str,
        job: impl FnOnce(account::Session) -> Event + Send + 'static,
    ) {
        let Some(options) = account else {
            return;
        };
        self.spawn(name, move |notify| {
            let event = match account::session(&options) {
                Ok(session) => job(session),
                Err(err) => Event::Failed(err),
            };
            notify(Message::Event(event));
        });
    }

    fn start_lobby(&mut self, options: AccountOptions) {
        self.stop_lobby();
        let stop = Arc::new(AtomicBool::new(false));
        self.lobby = Some(Arc::clone(&stop));
        self.spawn("espera", move |notify| lobby(&options, &stop, &notify));
    }

    fn stop_lobby(&mut self) {
        if let Some(stop) = self.lobby.take() {
            stop.store(true, Ordering::Release);
        }
    }

    fn start_host(&mut self, options: AccountOptions, request: HostRequest, settings: &Settings) {
        let video = &settings.video;
        let (width, height) = video.max_resolution.size();
        let host = HostOptions {
            net: net_options(&options, settings),
            source: match request.source {
                SourceChoice::Screen => Source::Screen,
                SourceChoice::Synthetic => Source::Synthetic,
            },
            video: coroom_session::loopback::LoopbackConfig {
                width,
                height,
                fps: video.fps,
                bitrate_kbps: video.max_bitrate_kbps,
                frames: None,
                yuv444: video.chroma_444,
                hdr: video.hdr,
            },
            codec: match video.codec {
                CodecChoice::Auto => None,
                CodecChoice::Av1 => Some(coroom_session::codec::Codec::Av1),
                CodecChoice::Hevc => Some(coroom_session::codec::Codec::Hevc),
                CodecChoice::H264 => Some(coroom_session::codec::Codec::H264),
            },
            encoder: None,
            auto_allow: AutoAllow::default(),
            gamepad_to_keys: false,
            audio: HostAudioOptions {
                game: request.audio,
                voice: request.voice,
                mic: settings.audio.microphone.clone(),
                speaker: settings.audio.speaker.clone(),
                frame_ms: 10,
                noise_suppression: settings.audio.noise_suppression,
            },
            invites: Vec::new(),
        };
        let slot = Arc::clone(&self.host);
        let preview = VideoFeed::new(None, Arc::clone(&self.wake));
        self.spawn("sala", move |notify| {
            let started = load_identity(crate::ui::settings::explicit_data_dir().as_deref(), false)
                .and_then(|(identity, _)| start_host(identity, &host));
            let session = match started {
                Ok(session) => session,
                Err(err) => {
                    notify(Message::Event(Event::HostStarted(Err(err))));
                    return;
                }
            };
            let feed = Arc::clone(&preview);
            let preview_notify = Arc::clone(&notify);
            session.set_preview(Some(PreviewTap::new(Box::new(move |frame| match frame {
                Some(frame) => {
                    if let Ok(frame) = to_present_frame(frame) {
                        feed.push(frame, None, None);
                    }
                }
                None => preview_notify(Message::Event(Event::PreviewUnavailable)),
            }))));
            let events = session.events();
            let session = Arc::new(Mutex::new(session));
            *lock(&slot) = Some(Arc::clone(&session));
            notify(Message::Event(Event::HostStarted(Ok(preview))));
            forward_events(events, &notify, Event::Host, Event::HostEnded);
            drop(session);
        });
        // Estado de los invitados mientras haya sala.
        let slot = Arc::clone(&self.host);
        self.spawn("invitados", move |notify| {
            // Se espera a que la sala exista (o a que falle al abrirse).
            for _ in 0..600 {
                if lock(&slot).is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            loop {
                let Some(session) = lock(&slot).clone() else {
                    return;
                };
                let (finished, guests) = {
                    let session = lock(&session);
                    (session.is_finished(), session.guests())
                };
                if finished {
                    return;
                }
                notify(Message::Event(Event::HostGuests(guests)));
                thread::sleep(HOST_STATUS_PERIOD);
            }
        });
    }

    /// Entra en una sala (`identity`: la del equipo si `None`).
    pub fn start_join(&mut self, identity: Option<Identity>, options: JoinOptions) {
        let feed = VideoFeed::new(self.importer.clone(), Arc::clone(&self.wake));
        let output = self.output.clone();
        let slot = Arc::clone(&self.guest);
        let cancel = Arc::new(AtomicBool::new(false));
        self.join_cancel = Some(Arc::clone(&cancel));
        self.spawn("invitado", move |notify| {
            let identity = match identity {
                Some(identity) => Ok(identity),
                None => load_identity(crate::ui::settings::explicit_data_dir().as_deref(), false)
                    .map(|(identity, _)| identity),
            };
            let sink = Box::new(FeedSink::new(Arc::clone(&feed)));
            let started =
                identity.and_then(|identity| start_guest(identity, &options, sink, output));
            let session = match started {
                Ok(_) | Err(_) if cancel.load(Ordering::Relaxed) => {
                    if let Ok(mut session) = started {
                        session.close();
                    }
                    feed.close();
                    return;
                }
                Ok(session) => session,
                Err(err) => {
                    feed.close();
                    notify(Message::Event(Event::GuestStarted(Err(err))));
                    return;
                }
            };
            let controls = GuestControls {
                input: session.input(),
                audio: session.audio(),
                push_to_talk: options.audio.ptt.clone(),
            };
            let events = session.events();
            let session = Arc::new(Mutex::new(session));
            *lock(&slot) = Some(GuestRun {
                session: Arc::clone(&session),
                controls: controls.clone(),
            });
            notify(Message::Event(Event::GuestStarted(Ok(Arc::clone(&feed)))));
            let stats_notify = Arc::clone(&notify);
            let stats_session = Arc::downgrade(&session);
            let stats_feed = Arc::clone(&feed);
            let poller = thread::Builder::new()
                .name("coroom-estadisticas".into())
                .spawn(move || {
                    while let Some(session) = stats_session.upgrade() {
                        let (finished, summary) = {
                            let session = lock(&session);
                            (session.is_finished(), session.summary())
                        };
                        if finished {
                            return;
                        }
                        stats_notify(Message::Event(Event::GuestStats(Box::new(stats(
                            summary,
                            &controls,
                            &stats_feed,
                        )))));
                        drop(session);
                        thread::sleep(GUEST_STATS_PERIOD);
                    }
                });
            drop(poller);
            forward_events(events, &notify, Event::Guest, Event::GuestEnded);
            feed.close();
            drop(session);
        });
    }

    /// `coroom probe` con ventana: el pipeline local presentando en la ventana.
    pub fn start_probe(&mut self, options: ProbeOptions) -> Arc<VideoFeed> {
        let feed = VideoFeed::new(self.importer.clone(), Arc::clone(&self.wake));
        let ledger = Arc::new(LatencyLedger::new());
        let sink = Box::new(FeedSink::new(Arc::clone(&feed)));
        match start_pipeline_with(&options, sink, Arc::clone(&ledger), self.output.clone()) {
            Ok(pipeline) => {
                *lock(&self.local) = Some(LocalRun {
                    pipeline: Some(pipeline),
                    ledger: Arc::clone(&ledger),
                });
                (self.notify)(Message::Event(Event::GuestStarted(Ok(Arc::clone(&feed)))));
                let local = Arc::clone(&self.local);
                let stats_feed = Arc::clone(&feed);
                self.spawn("prueba", move |notify| {
                    loop {
                        let finished = lock(&local)
                            .as_ref()
                            .and_then(|run| run.pipeline.as_ref())
                            .map(|pipeline| (pipeline.is_finished(), pipeline.failure()));
                        let Some((finished, failure)) = finished else {
                            return;
                        };
                        if finished {
                            notify(Message::Event(Event::GuestEnded(
                                failure.map(|err| t!("backend-la-prueba-se-detuvo-err", err = err)),
                            )));
                            return;
                        }
                        let summary = ledger.summary();
                        notify(Message::Event(Event::GuestStats(Box::new(Stats {
                            summary,
                            allowed: Default::default(),
                            mic_muted: false,
                            game_muted: false,
                            voice: false,
                            audio_status: t!("backend-sin-audio-en-la-prueba-local").to_owned(),
                            window_copies: stats_feed.cpu_copies(),
                        }))));
                        thread::sleep(GUEST_STATS_PERIOD);
                    }
                });
            }
            Err(err) => {
                feed.close();
                (self.notify)(Message::Event(Event::GuestStarted(Err(err))));
            }
        }
        feed
    }

    /// Detiene la prueba local y da su resumen.
    pub fn finish_probe(&mut self) -> Option<Result<LatencySummary, String>> {
        let mut run = lock(&self.local).take()?;
        let pipeline = run.pipeline.take()?;
        let failure = pipeline.failure();
        let summary = pipeline.stop();
        drop(run.ledger);
        Some(match failure {
            Some(err) => Err(t!("backend-la-prueba-se-detuvo-err", err = err)),
            None => Ok(summary),
        })
    }

    /// Cierra todo al salir (espera a que las sesiones terminen). Devuelve el resumen del
    /// invitado, si había sesión, y el motivo si terminó por un error.
    pub fn shutdown(&mut self) -> Option<Result<LatencySummary, String>> {
        self.stop_lobby();
        if let Some(session) = lock(&self.host).take() {
            lock(&session).close();
        }
        let guest = lock(&self.guest).take().map(|run| {
            let mut session = lock(&run.session);
            let reason = session.end_reason();
            session.close();
            match reason {
                Some(err) => Err(err.to_string()),
                None => Ok(session.summary()),
            }
        });
        guest.or_else(|| self.finish_probe())
    }
}

/// Estadísticas del invitado para la interfaz.
fn stats(summary: LatencySummary, controls: &GuestControls, feed: &VideoFeed) -> Stats {
    let audio = &controls.audio;
    Stats {
        summary,
        allowed: controls.input.allowed(),
        mic_muted: audio.mic_muted(),
        game_muted: audio.game_muted(),
        voice: audio.voice_enabled() && audio.voice_allowed(),
        audio_status: audio.status_label(),
        window_copies: feed.cpu_copies(),
    }
}

/// Reenvía los eventos de una sesión hasta el final.
fn forward_events(
    events: crossbeam_channel::Receiver<SessionEvent>,
    notify: &Notify,
    each: fn(SessionEvent) -> Event,
    end: fn(Option<String>) -> Event,
) {
    while let Ok(event) = events.recv() {
        if let SessionEvent::Ended(reason) = &event {
            let reason = reason.as_ref().map(SessionError::to_string);
            notify(Message::Event(end(reason)));
            return;
        }
        notify(Message::Event(each(event)));
    }
    notify(Message::Event(end(None)));
}

/// Opciones de cuenta de un servidor conocido.
pub fn account_options(server: &ServerEntry) -> AccountOptions {
    AccountOptions {
        server: Some(server.url.clone()),
        trust_fingerprint: server.fingerprint.clone(),
        tcp_port: server.tcp_port,
        data_dir: super::settings::explicit_data_dir(),
        headless: false,
    }
}

fn net_options(options: &AccountOptions, settings: &Settings) -> NetOptions {
    NetOptions {
        server: options.server.clone(),
        dev_token: None,
        trust_fingerprint: options.trust_fingerprint.clone(),
        force_relay: settings.network.force_relay,
        data_dir: options.data_dir.clone(),
        headless: false,
        local_port: Some(settings.network.local_port).filter(|port| *port != 0),
        #[cfg(debug_assertions)]
        insecure_no_sframe: false,
    }
}

/// Opciones para entrar en `code` con los ajustes.
pub fn join_options(options: &AccountOptions, settings: &Settings, code: String) -> JoinOptions {
    let audio = &settings.audio;
    JoinOptions {
        net: net_options(options, settings),
        code,
        first_invitation: false,
        frames: None,
        decoder: None,
        input_script: None,
        audio: JoinAudioOptions {
            disabled: false,
            mute_game: false,
            voice: audio.voice,
            ptt: (audio.voice_mode == VoiceMode::PushToTalk)
                .then(|| audio.push_to_talk_key.clone()),
            mic: audio.microphone.clone(),
            speaker: audio.speaker.clone(),
            noise_suppression: audio.noise_suppression,
        },
        yuv444: settings.video.chroma_444,
        hdr: settings.video.hdr,
    }
}

fn account_info(server: ServerEntry, signed: account::SignedIn) -> AccountInfo {
    AccountInfo {
        server,
        username: signed.credentials.username,
        display_name: signed.credentials.display_name,
        user_id: signed.credentials.user_id,
        device: signed.device,
        fingerprint: signed.fingerprint,
    }
}

/// Recupera la sesión del último servidor (si hay credenciales guardadas).
fn restore(servers: &[ServerEntry]) -> Event {
    let store = AccountStore::new(None, false);
    let Some(last) = store.last_server() else {
        return Event::NoSavedSession;
    };
    let server = servers
        .iter()
        .find(|server| server.url == last)
        .cloned()
        .unwrap_or(ServerEntry {
            url: last,
            fingerprint: None,
            tcp_port: None,
            username: None,
        });
    let mut options = account_options(&server);
    match store.load(&server.url) {
        Ok(Some(credentials)) => {
            // La huella y el puerto que se guardaron al iniciar sesión.
            options.trust_fingerprint = options
                .trust_fingerprint
                .or(credentials.trust_fingerprint.clone());
            options.tcp_port = options.tcp_port.or(credentials.tcp_port);
        }
        Ok(None) => return Event::NoSavedSession,
        Err(err) => return Event::SignedIn(Err(err)),
    }
    let server = ServerEntry {
        fingerprint: options.trust_fingerprint.clone(),
        tcp_port: options.tcp_port,
        ..server
    };
    Event::SignedIn(account::resume(&options).map(|(_, signed)| account_info(server, signed)))
}

/// Sala de espera: con la cuenta abierta y sin sesión, escucha invitaciones y presencia (y así
/// los amigos te ven conectado). Si la señalización se cae, vuelve a intentarlo.
fn lobby(options: &AccountOptions, stop: &AtomicBool, notify: &Notify) {
    while !stop.load(Ordering::Acquire) {
        if let Err(err) = lobby_once(options, stop, notify) {
            tracing::debug!(error = %err, "sala de espera");
        }
        let deadline = std::time::Instant::now() + LOBBY_RETRY;
        while std::time::Instant::now() < deadline {
            if stop.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
}

fn lobby_once(options: &AccountOptions, stop: &AtomicBool, notify: &Notify) -> Result<(), String> {
    let session = account::session(options)?;
    let (identity, _) = load_identity(options.data_dir.as_deref(), options.headless)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    runtime.block_on(async {
        let endpoint = Endpoint::bind(&identity, None, EndpointOptions::default())
            .map_err(|err| err.to_string())?;
        let mut signal = SignalClient::connect(
            &session.credentials.server,
            &session.credentials.token,
            &endpoint,
            &session.trust,
        )
        .await
        .map_err(|err| err.to_string())?;
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        let result = loop {
            tokio::select! {
                event = signal.next_event() => match event {
                    Ok(Some(SignalMessage::Invited { from_user, from_name, code })) => {
                        notify(Message::Event(Event::Invited(Invitation {
                            from_user,
                            from_name,
                            code,
                        })));
                    }
                    Ok(Some(SignalMessage::Presence { user, online })) => {
                        notify(Message::Event(Event::Presence { user, online }));
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break Err(t!("backend-el-servidor-cerro-la-senalizacion").to_owned()),
                    Err(err) => break Err(err.to_string()),
                },
                _ = tick.tick() => {
                    if stop.load(Ordering::Acquire) {
                        break Ok(());
                    }
                }
            }
        };
        signal.close();
        endpoint.close();
        result
    })
}
