//! Lo que la interfaz no hace en su hilo: la API del servidor, el llavero, las sesiones remotas,
//! la sala de espera (invitaciones y presencia), el diagnóstico y los dispositivos de audio.
//!
//! [`Backend::execute`] recibe cada [`Effect`] de [`super::app::App::update`] y lo hace en un hilo
//! propio (el llavero de Linux y `RemoteSession::start`/`close` bloquean, y no deben ir dentro de
//! un runtime de `tokio`), y devuelve el resultado con `notify` como [`Message::Event`]. Las
//! sesiones mandan sus eventos (un hilo los reenvía) y su estado cada 250-500 ms (otro hilo lo
//! pide), así la ventana nunca espera a la red.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

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
use super::screens::host::{ShareOptions, SourceChoice};
use super::settings::{CodecChoice, DecoderChoice, ServerEntry, Settings, VoiceMode};
use super::video::{FeedSink, VideoFeed, Wake};
use crate::account::{self, AccountOptions, AccountStore};
use crate::probe::{ProbeOptions, Source, start_pipeline_with, to_present_frame};
use crate::remote::{
    AutoAllow, HostAudioOptions, HostOptions, JoinAudioOptions, JoinOptions, NetOptions,
    host_encoders, load_identity, prepare_host, share_source, start_guest, start_prepared_host,
};

/// Entrega un mensaje a la interfaz (desde cualquier hilo).
pub type Notify = Arc<dyn Fn(Message) + Send + Sync>;

/// Cada cuánto se piden las estadísticas del invitado.
pub const GUEST_STATS_PERIOD: Duration = Duration::from_millis(250);

/// Cada cuánto se pide el estado de los invitados del anfitrión.
pub const HOST_STATUS_PERIOD: Duration = Duration::from_millis(500);

/// Espera antes de reabrir la sala de espera si se cae.
const LOBBY_RETRY: Duration = Duration::from_secs(5);

/// Lo más que se espera a que la sala conecte con el servidor al abrirla; después se avisa y se
/// vuelve a la sala de espera (si arranca tarde, se cierra sola).
pub const HOST_START_TIMEOUT: Duration = Duration::from_secs(20);

/// Lo más que se espera al salir a que cada sesión se cierre.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

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
    /// Sala de espera en marcha: su parada y el servidor al que escucha.
    lobby: Option<(Arc<AtomicBool>, Option<String>)>,
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
    // Lo que descartó la comprobación de imagen del sondeo, en el registro (una vez por proceso:
    // el sondeo se hace una vez).
    static LOGGED: std::sync::Once = std::sync::Once::new();
    LOGGED.call_once(|| {
        for line in &coroom_session::codec::Capabilities::probe().rejected {
            tracing::warn!("descartado por la comprobación de imagen: {line}");
        }
    });
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
        self.spawn_or(name, Event::Failed, job);
    }

    /// Como [`Backend::spawn`]; si la tarea entra en pánico, avisa con `on_panic` (la sala o la
    /// sesión del invitado, para que la pantalla no se quede esperando).
    fn spawn_or(
        &self,
        name: &str,
        on_panic: fn(String) -> Event,
        job: impl FnOnce(Notify) + Send + 'static,
    ) {
        spawn_job(Arc::clone(&self.notify), name, on_panic, job);
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
            Effect::Resume(server) => {
                self.spawn("recuperar", move |notify| {
                    notify(Message::Event(resume_server(server)));
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
                    tracing::info!("cerrando la sala");
                    close_detached(session);
                }
            }
            Effect::SwitchSource(request) => self.switch_source(request, &app.settings),
            Effect::SetAppAudio(audio) => {
                if let coroom_session::audio::GameCapture::Apps(filter) = audio
                    && let Some(session) = lock(&self.host).clone()
                    && let Err(err) = lock(&session).set_app_audio(filter)
                {
                    (self.notify)(Message::Event(Event::Failed(err.to_string())));
                }
            }
            Effect::SetVideo => self.set_video(&app.settings),
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
                    tracing::info!("saliendo de la sala");
                    close_detached(run.session);
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
            Effect::LoadShareOptions(source) => self.spawn("compartir", move |notify| {
                notify(Message::Event(Event::ShareOptions(share_options(source))));
            }),
            Effect::PickShare(source) => self.spawn("elegir", move |notify| {
                let picked = match coroom_session::capture::pick_share(source.kind()) {
                    Ok(picked) => Ok(picked),
                    // Cerrar el selector no es un error: se queda como estaba.
                    Err(coroom_session::capture::CaptureError::PermissionDenied) => Ok(None),
                    Err(err) => Err(err.to_string()),
                };
                notify(Message::Event(Event::SharePicked(source, picked)));
            }),
            Effect::LoadVideoSupport => {
                let hdr_display = self.hdr_display;
                self.spawn("video", move |notify| {
                    notify(Message::Event(Event::VideoSupport(video_support(
                        hdr_display,
                    ))));
                });
            }
            Effect::Copy(_)
            | Effect::SetFullscreen(_)
            | Effect::Quit
            | Effect::SetImmersive(_)
            | Effect::SetPadChoice(_) => {}
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

    /// Abre la sala de espera; si ya escucha al mismo servidor, la deja como está (cerrarla y
    /// abrirla haría que los amigos te vieran desconectado un momento).
    fn start_lobby(&mut self, options: AccountOptions) {
        if self
            .lobby
            .as_ref()
            .is_some_and(|(_, server)| *server == options.server)
        {
            return;
        }
        self.stop_lobby();
        let stop = Arc::new(AtomicBool::new(false));
        self.lobby = Some((Arc::clone(&stop), options.server.clone()));
        tracing::info!("sala de espera abierta");
        self.spawn("espera", move |notify| lobby(&options, &stop, &notify));
    }

    fn stop_lobby(&mut self) {
        if let Some((stop, _)) = self.lobby.take() {
            tracing::info!("sala de espera cerrada");
            stop.store(true, Ordering::Release);
        }
    }

    /// Compartir otra cosa con la sala abierta: la captura nueva arranca en su hilo (en Linux
    /// puede preguntar el portal) y la sesión la adopta; si no arranca, sigue la de antes.
    fn switch_source(&self, request: HostRequest, settings: &Settings) {
        let Some(session) = lock(&self.host).clone() else {
            return;
        };
        let control = match lock(&session).host_control() {
            Ok(control) => control,
            Err(err) => {
                (self.notify)(Message::Event(Event::SourceSwitched(Err(err.to_string()))));
                return;
            }
        };
        let source = host_source(request.source);
        let video = host_video(settings);
        tracing::info!(source = ?request.source, share = ?request.share.kind, "cambiando la fuente");
        self.spawn("fuente", move |notify| {
            let switched =
                share_source(source, &request.share, video).and_then(|(capturer, share)| {
                    control
                        .switch_capture(capturer, share.map(coroom_session::remote::eis_provider))
                        .map_err(|err| err.to_string())
                });
            if switched.is_ok()
                && let coroom_session::audio::GameCapture::Apps(filter) = request.audio
                && let Err(err) = control.set_app_audio(filter)
            {
                tracing::debug!(error = %err, "audio de la fuente nueva");
            }
            match &switched {
                Ok(()) => tracing::info!("fuente cambiada"),
                Err(err) => tracing::warn!(error = %err, "no se pudo cambiar la fuente"),
            }
            notify(Message::Event(Event::SourceSwitched(switched)));
        });
    }

    /// Códec, codificador y bitrate máximo de los ajustes a la sala abierta.
    fn set_video(&self, settings: &Settings) {
        let Some(session) = lock(&self.host).clone() else {
            return;
        };
        let video = &settings.video;
        let (codec, encoder) = (codec_preference(video.codec), video.encoder.backend());
        let max_kbps = video.max_bitrate_kbps;
        let control = lock(&session).host_control();
        self.spawn("video", move |notify| {
            // El sondeo de codificadores ya está hecho (se guarda): esto no abre nada.
            let changed = host_encoders(codec, encoder).and_then(|encoders| {
                control
                    .and_then(|control| {
                        control.set_video(coroom_session::remote::VideoChange {
                            encoders: Some(encoders),
                            max_kbps: Some(max_kbps),
                        })
                    })
                    .map_err(|err| err.to_string())
            });
            let ok = changed.is_ok();
            tracing::info!(?codec, ?encoder, max_kbps, ok, "vídeo de la sala cambiado");
            if let Err(err) = changed {
                notify(Message::Event(Event::Failed(err)));
            }
        });
    }

    fn start_host(&mut self, options: AccountOptions, request: HostRequest, settings: &Settings) {
        let video = &settings.video;
        let (width, height) = video.max_resolution.size();
        let host = HostOptions {
            net: net_options(&options, settings),
            source: host_source(request.source),
            share: request.share,
            video: host_video(settings),
            codec: codec_preference(video.codec),
            encoder: video.encoder.backend(),
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
        tracing::info!(
            source = ?request.source,
            share = ?host.share.kind,
            codec = ?host.codec,
            encoder = ?host.encoder,
            width,
            height,
            fps = video.fps,
            bitrate_kbps = video.max_bitrate_kbps,
            yuv444 = video.chroma_444,
            hdr = video.hdr,
            "abriendo la sala"
        );
        let slot = Arc::clone(&self.host);
        let preview = VideoFeed::new(None, Arc::clone(&self.wake));
        let on_panic: fn(String) -> Event = |reason| Event::HostEnded(Some(reason));
        self.spawn_or("sala", on_panic, move |notify| {
            let prepared =
                load_identity(crate::ui::settings::explicit_data_dir().as_deref(), false)
                    .and_then(|(identity, _)| prepare_host(identity, &host));
            let mut session = match prepared {
                Ok(session) => session,
                Err(err) => {
                    tracing::warn!(error = %err, "no se pudo preparar la sala");
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
            // La captura ya está elegida (el diálogo del sistema no cuenta): desde aquí, si el
            // servidor no contesta, se avisa en vez de quedarse en «Abriendo la sala…».
            let gate = StartGate::new();
            gate.watch(HOST_START_TIMEOUT, Arc::clone(&notify), || {
                Event::HostStarted(Err(t!(
                    "backend-la-sala-no-respondio",
                    secs = HOST_START_TIMEOUT.as_secs()
                )))
            });
            let started = start_prepared_host(&mut session);
            if !gate.settle() {
                tracing::warn!("la sala arrancó después del plazo; se cierra");
                session.close();
                return;
            }
            if let Err(err) = started {
                tracing::warn!(error = %err, "no se pudo abrir la sala");
                notify(Message::Event(Event::HostStarted(Err(err))));
                return;
            }
            tracing::info!("sala abierta");
            let events = session.events();
            let session = Arc::new(Mutex::new(session));
            // La de antes (si terminó sola y nadie la quitó) se cierra aparte, nunca bajo el
            // cerrojo: cerrar espera a sus hilos.
            let old = lock(&slot).replace(Arc::clone(&session));
            if let Some(old) = old {
                close_detached(old);
            }
            notify(Message::Event(Event::HostStarted(Ok(preview))));
            poll_host_guests(Arc::downgrade(&session), Arc::clone(&notify));
            let reason = forward_events(events, &notify, Event::Host, &session, "host");
            // Terminó (sola o porque se cerró): sale del hueco si sigue siendo la de ahora, para
            // que la próxima sala no tropiece con ella.
            drop(take_if(&slot, |current| Arc::ptr_eq(current, &session)));
            close_detached(session);
            tracing::info!(
                reason = reason.as_deref().unwrap_or("cierre normal"),
                "sala cerrada"
            );
            notify(Message::Event(Event::HostEnded(reason)));
        });
    }

    /// Entra en una sala (`identity`: la del equipo si `None`).
    pub fn start_join(&mut self, identity: Option<Identity>, options: JoinOptions) {
        let feed = VideoFeed::new(self.importer.clone(), Arc::clone(&self.wake));
        let output = self.output.clone();
        let slot = Arc::clone(&self.guest);
        let cancel = Arc::new(AtomicBool::new(false));
        self.join_cancel = Some(Arc::clone(&cancel));
        tracing::info!(
            decoder = ?options.decoder,
            yuv444 = options.yuv444,
            hdr = options.hdr,
            "entrando en una sala"
        );
        let on_panic: fn(String) -> Event = |reason| Event::GuestEnded(Some(reason));
        self.spawn_or("invitado", on_panic, move |notify| {
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
                    tracing::warn!(error = %err, "no se pudo entrar en la sala");
                    feed.close();
                    notify(Message::Event(Event::GuestStarted(Err(err))));
                    return;
                }
            };
            tracing::info!("dentro de la sala");
            let controls = GuestControls {
                input: session.input(),
                audio: session.audio(),
                push_to_talk: options.audio.ptt.clone(),
            };
            let events = session.events();
            let session = Arc::new(Mutex::new(session));
            let old = lock(&slot).replace(GuestRun {
                session: Arc::clone(&session),
                controls: controls.clone(),
            });
            if let Some(old) = old {
                close_detached(old.session);
            }
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
            let reason = forward_events(events, &notify, Event::Guest, &session, "guest");
            drop(take_if(&slot, |run| Arc::ptr_eq(&run.session, &session)));
            close_detached(session);
            feed.close();
            tracing::info!(
                reason = reason.as_deref().unwrap_or("cierre normal"),
                "fuera de la sala"
            );
            notify(Message::Event(Event::GuestEnded(reason)));
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
        let host = lock(&self.host).take();
        if let Some(session) = host {
            close_within(SHUTDOWN_WAIT, move || lock(&session).close());
        }
        let run = lock(&self.guest).take();
        let guest = run.and_then(|run| {
            close_within(SHUTDOWN_WAIT, move || {
                let mut session = lock(&run.session);
                let reason = session.end_reason();
                session.close();
                match reason {
                    Some(err) => Err(err.to_string()),
                    None => Ok(session.summary()),
                }
            })
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

/// Reenvía los eventos de una sesión hasta el final (y los deja en el registro). Devuelve el
/// motivo si terminó por un error: el de la sesión o, si no dice ninguno, el del pipeline (un
/// codificador o la captura que fallan), para que la interfaz no diga «Has dejado de compartir».
fn forward_events(
    events: crossbeam_channel::Receiver<SessionEvent>,
    notify: &Notify,
    each: fn(SessionEvent) -> Event,
    session: &Mutex<RemoteSession>,
    role: &'static str,
) -> Option<String> {
    let mut reason = None;
    while let Ok(event) = events.recv() {
        match &event {
            SessionEvent::Warning(_) | SessionEvent::RelayUnavailable(_) => {
                tracing::warn!(role, %event, "aviso de la sesión");
            }
            _ => tracing::info!(role, %event, "evento de la sesión"),
        }
        if let SessionEvent::Ended(ended) = event {
            reason = ended;
            break;
        }
        notify(Message::Event(each(event)));
    }
    reason
        .or_else(|| {
            // Sin esperar: si alguien la está cerrando, el motivo ya llegó con el evento.
            let session = match session.try_lock() {
                Ok(session) => session,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => return None,
            };
            session.end_reason()
        })
        .as_ref()
        .map(SessionError::to_string)
}

/// Lanza `job` en un hilo propio; si entra en pánico, avisa con `on_panic` y el motivo.
fn spawn_job(
    notify: Notify,
    name: &str,
    on_panic: fn(String) -> Event,
    job: impl FnOnce(Notify) + Send + 'static,
) {
    let task = name.to_owned();
    let inner = Arc::clone(&notify);
    let spawned = thread::Builder::new()
        .name(format!("coroom-{name}"))
        .spawn(move || {
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| job(Arc::clone(&inner)))) {
                let err = panic_message(panic.as_ref());
                tracing::error!(task, error = %err, "una tarea entró en pánico");
                inner(Message::Event(on_panic(t!(
                    "backend-fallo-interno",
                    task = task,
                    err = err
                ))));
            }
        });
    if let Err(err) = spawned {
        notify(Message::Event(Event::Failed(t!(
            "backend-no-se-pudo-arrancar-una-tarea",
            err = err
        ))));
    }
}

/// El texto de un pánico.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "?".to_owned())
}

/// Saca lo que hay en `slot` si `is_mine` (y lo devuelve: se suelta fuera del cerrojo).
fn take_if<T>(slot: &Mutex<Option<T>>, is_mine: impl FnOnce(&T) -> bool) -> Option<T> {
    let mut slot = lock(slot);
    if slot.as_ref().is_some_and(is_mine) {
        slot.take()
    } else {
        None
    }
}

/// Cierra la sesión en un hilo aparte: `close` espera a sus hilos y puede tardar (o no volver si
/// alguno se quedó colgado), y nunca debe hacerlo bajo un cerrojo ni en el hilo de la ventana.
fn close_detached(session: Arc<Mutex<RemoteSession>>) {
    let spawned = thread::Builder::new()
        .name("coroom-cerrar".into())
        .spawn(move || lock(&session).close());
    if let Err(err) = spawned {
        tracing::warn!(error = %err, "no se pudo cerrar la sesión aparte");
    }
}

/// Hace `job` en otro hilo y espera su resultado como mucho `wait` (al salir de la aplicación).
fn close_within<T: Send + 'static>(
    wait: Duration,
    job: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = thread::Builder::new()
        .name("coroom-cerrar".into())
        .spawn(move || {
            let _ = tx.send(job());
        });
    if spawned.is_err() {
        return None;
    }
    let result = rx.recv_timeout(wait).ok();
    if result.is_none() {
        tracing::warn!(segundos = wait.as_secs(), "una sesión no se cerró a tiempo");
    }
    result
}

/// Estado de los invitados del anfitrión cada [`HOST_STATUS_PERIOD`] mientras la sala siga.
fn poll_host_guests(session: std::sync::Weak<Mutex<RemoteSession>>, notify: Notify) {
    let spawned = thread::Builder::new()
        .name("coroom-invitados".into())
        .spawn(move || {
            while let Some(session) = session.upgrade() {
                let (finished, guests) = {
                    let session = lock(&session);
                    (session.is_finished(), session.guests())
                };
                drop(session);
                if finished {
                    return;
                }
                notify(Message::Event(Event::HostGuests(guests)));
                thread::sleep(HOST_STATUS_PERIOD);
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(error = %err, "sin estado de los invitados");
    }
}

/// Quién decide cómo acabó un arranque: el arranque o el plazo, el primero que llegue.
#[derive(Clone)]
struct StartGate(Arc<(Mutex<bool>, Condvar)>);

impl StartGate {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }

    /// Si en `timeout` nadie llamó a [`StartGate::settle`], avisa con `event` (desde otro hilo).
    fn watch(
        &self,
        timeout: Duration,
        notify: Notify,
        event: impl FnOnce() -> Event + Send + 'static,
    ) {
        let gate = self.clone();
        let spawned = thread::Builder::new()
            .name("coroom-plazo".into())
            .spawn(move || {
                let (decided, wake) = &*gate.0;
                let deadline = Instant::now() + timeout;
                let mut done = lock(decided);
                while !*done {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    done = wake
                        .wait_timeout(done, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                if !*done {
                    *done = true;
                    drop(done);
                    tracing::warn!(?timeout, "arranque fuera de plazo");
                    notify(Message::Event(event()));
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(error = %err, "sin plazo para el arranque");
        }
    }

    /// El arranque terminó: `true` si llegó a tiempo (y el plazo ya no avisará).
    fn settle(&self) -> bool {
        let (decided, wake) = &*self.0;
        let mut done = lock(decided);
        if *done {
            return false;
        }
        *done = true;
        wake.notify_all();
        true
    }
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

/// Monitores o ventanas y aplicaciones que suenan, para el paso 1 del anfitrión.
fn share_options(source: SourceChoice) -> ShareOptions {
    let (targets, targets_error) = match source {
        SourceChoice::Synthetic => (Vec::new(), None),
        SourceChoice::Screen | SourceChoice::App => {
            match coroom_session::capture::share_targets(source.kind()) {
                Ok(targets) => (targets, None),
                Err(err) => {
                    tracing::warn!(%err, ?source, "no se pudieron listar los destinos");
                    (Vec::new(), Some(err.to_string()))
                }
            }
        }
    };
    ShareOptions {
        source,
        targets,
        targets_error,
        apps: coroom_session::audio::playing_apps().map_err(|err| err.to_string()),
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
        decoder: match settings.video.decoder {
            DecoderChoice::Auto => None,
            DecoderChoice::Software => Some(coroom_session::codec::DecoderBackend::Software),
        },
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
    resume_from(&store, server)
}

/// Recupera la sesión guardada de `server` y lo deja como el último (el que se recupera al abrir).
fn resume_server(server: ServerEntry) -> Event {
    let store = AccountStore::new(None, false);
    let event = resume_from(&store, server.clone());
    if matches!(event, Event::SignedIn(Ok(_))) {
        store.set_last_server(&server.url);
    }
    event
}

fn resume_from(store: &AccountStore, server: ServerEntry) -> Event {
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

/// Captura que usa la sala para la fuente elegida en la pantalla.
fn host_source(source: SourceChoice) -> Source {
    match source {
        SourceChoice::Screen | SourceChoice::App => Source::Screen,
        SourceChoice::Synthetic => Source::Synthetic,
    }
}

/// Vídeo de la sala según los ajustes.
fn host_video(settings: &Settings) -> coroom_session::loopback::LoopbackConfig {
    let video = &settings.video;
    let (width, height) = video.max_resolution.size();
    coroom_session::loopback::LoopbackConfig {
        width,
        height,
        fps: video.fps,
        bitrate_kbps: video.max_bitrate_kbps,
        frames: None,
        yuv444: video.chroma_444,
        hdr: video.hdr,
    }
}

/// Códec preferido de los ajustes (`None`: el mejor común).
fn codec_preference(choice: CodecChoice) -> Option<coroom_session::codec::Codec> {
    match choice {
        CodecChoice::Auto => None,
        CodecChoice::Av1 => Some(coroom_session::codec::Codec::Av1),
        CodecChoice::Hevc => Some(coroom_session::codec::Codec::Hevc),
        CodecChoice::H264 => Some(coroom_session::codec::Codec::H264),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un `notify` que guarda los mensajes en un canal.
    fn recorder() -> (Notify, crossbeam_channel::Receiver<Message>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let notify: Notify = Arc::new(move |message| {
            let _ = tx.send(message);
        });
        (notify, rx)
    }

    /// Una tarea que entra en pánico avisa con el motivo (la pantalla no se queda esperando).
    #[test]
    fn a_panicking_job_reports_instead_of_vanishing() {
        let (notify, rx) = recorder();
        let on_panic: fn(String) -> Event = |reason| Event::HostEnded(Some(reason));
        spawn_job(notify, "sala", on_panic, |_| panic!("códec roto"));
        let message = rx.recv_timeout(Duration::from_secs(10)).expect("aviso");
        let Message::Event(Event::HostEnded(Some(reason))) = message else {
            panic!("se esperaba el final de la sala: {message:?}");
        };
        assert!(reason.contains("códec roto"), "{reason}");
        assert!(reason.contains("sala"), "{reason}");
    }

    /// El plazo avisa si el arranque no termina; un arranque que llega después ya no cuenta, y
    /// uno a tiempo no deja aviso.
    #[test]
    fn the_start_timeout_wins_only_when_the_start_is_late() {
        let (notify, rx) = recorder();
        let late = StartGate::new();
        late.watch(Duration::from_millis(50), Arc::clone(&notify), || {
            Event::HostStarted(Err("plazo".into()))
        });
        let message = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("aviso del plazo");
        assert!(matches!(
            message,
            Message::Event(Event::HostStarted(Err(ref err))) if err == "plazo"
        ));
        assert!(!late.settle(), "el arranque tardío se descarta");

        let on_time = StartGate::new();
        on_time.watch(Duration::from_millis(200), notify, || {
            Event::HostStarted(Err("plazo".into()))
        });
        assert!(on_time.settle());
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "sin aviso"
        );
    }

    /// La sesión que termina sola sale del hueco, pero no se lleva la que ya la sustituyó.
    #[test]
    fn an_ended_session_leaves_the_slot_only_if_it_is_still_there() {
        let first = Arc::new(1);
        let second = Arc::new(2);
        let slot = Mutex::new(Some(Arc::clone(&second)));
        assert!(take_if(&slot, |current| Arc::ptr_eq(current, &first)).is_none());
        assert!(lock(&slot).is_some(), "la nueva sigue");
        assert!(take_if(&slot, |current| Arc::ptr_eq(current, &second)).is_some());
        assert!(lock(&slot).is_none());
    }

    /// Cerrar al salir no espera para siempre a una sesión colgada.
    #[test]
    fn closing_on_exit_is_bounded() {
        let started = Instant::now();
        let result = close_within(Duration::from_millis(50), || {
            thread::sleep(Duration::from_secs(5));
        });
        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(close_within(Duration::from_secs(5), || 7), Some(7));
    }
}
