//! Capturas de cada pantalla, fuera de pantalla y con datos de ejemplo (ficticios: «Ana», «Bea»,
//! `coroom.example.com`).
//!
//! [`scenes`] monta la aplicación en cada estado que interesa revisar (llevándola con sus propios
//! mensajes y eventos, como en uso real) y [`ShotRenderer`] la dibuja con `iced_wgpu` en una
//! textura, sin ventana ni superficie, con el mismo dispositivo que abriría la ventana. El test
//! `tests/shots.rs` las guarda en PNG (en `COROOM_SHOTS_DIR` si está definida).

use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_render::wgpu::{GpuContext, create_instance};
use coroom_render::{CursorImage, CursorOverlay, PresentFrame, PresentImage};
use coroom_session::latency::{InputLatency, LatencyLedger, LinkSummary, Stage};
use coroom_session::remote::{GuestStatus, QualityLevel, SessionEvent};
use coroom_session::{GuestId, Permissions};
use coroom_transport::api::ServerProbe;
use coroom_transport::crypto::SafetyNumber;
use coroom_transport::proto::api::{Device, Devices, Friend, FriendRequest, Friends, Usage};
use coroom_transport::quic::Path;
use coroom_ui::font::Fonts;
use coroom_ui::theme::Mode;
use iced_core::widget::operation::{self, Operation};
use iced_core::{Event as UiEvent, Font, Pixels, Size, mouse, renderer, window};
use iced_runtime::user_interface::{self, UserInterface};
use iced_wgpu::graphics::{Shell, Viewport};
use iced_wgpu::{Engine, Renderer, wgpu};

use super::app::{
    AccountInfo, App, AudioDevice, Dialog, Event, Invitation, Load, Message, Page, Screen,
};
use super::screens::guest::{self, Phase, Stats};
use super::screens::login::{self, Probe, Step};
use super::screens::settings::SettingsPage;
use super::settings::{KnownPeer, ServerEntry, Settings, ThemeChoice};
use super::system::SystemAppearance;
use super::video::VideoFeed;

/// Tamaño lógico de las capturas (el de la ventana al abrirse).
pub const SIZE: (u32, u32) = (1180, 780);

/// Servidor de ejemplo.
pub const SERVER: &str = "https://coroom.example.com";

/// Una captura: nombre de fichero (sin extensión) y la aplicación en ese estado.
pub struct Scene {
    /// Nombre (`04-inicio-claro`).
    pub name: String,
    /// La aplicación.
    pub app: App,
}

/// Dibuja la aplicación en una textura.
pub struct ShotRenderer {
    renderer: Renderer,
    _gpu: GpuContext,
}

impl ShotRenderer {
    /// Abre un adaptador sin superficie (en contenedores, `lavapipe`).
    pub fn new() -> Result<Self, String> {
        let instance = create_instance(None);
        let gpu = GpuContext::new(&instance, None)
            .map_err(|err| format!("no hay adaptador gráfico: {err}"))?;
        let engine = Engine::new(
            &gpu.adapter,
            gpu.device.clone(),
            gpu.queue.clone(),
            wgpu::TextureFormat::Rgba8Unorm,
            None,
            Shell::headless(),
        );
        let renderer = Renderer::new(engine, Font::DEFAULT, Pixels(14.0));
        Ok(Self {
            renderer,
            _gpu: gpu,
        })
    }

    /// Dibuja `app` a `scale` (1.0, o 2.0 para HiDPI); devuelve el ancho, el alto y los píxeles
    /// RGBA.
    pub fn render(&mut self, app: &App, scale: f32) -> (u32, u32, Vec<u8>) {
        let width = (SIZE.0 as f32 * scale).round() as u32;
        let height = (SIZE.1 as f32 * scale).round() as u32;
        let viewport = Viewport::with_physical_size(Size::new(width, height), scale);
        let theme = app.theme;
        let mut cache = user_interface::Cache::new();
        // Dos pasadas: la primera asienta lo que depende del tamaño medido (desplazamientos,
        // menús), como en la ventana tras el primer evento.
        for _ in 0..2 {
            let mut interface = UserInterface::build(
                app.view(),
                viewport.logical_size(),
                cache,
                &mut self.renderer,
            );
            let mut messages: Vec<Message> = Vec::new();
            let mut clipboard = iced_core::clipboard::Null;
            let _ = interface.update(
                &[UiEvent::Window(window::Event::RedrawRequested(app.now))],
                mouse::Cursor::Unavailable,
                &mut self.renderer,
                &mut clipboard,
                &mut messages,
            );
            interface.draw(
                &mut self.renderer,
                &theme,
                &renderer::Style {
                    text_color: theme.text_color(),
                },
                mouse::Cursor::Unavailable,
            );
            cache = interface.into_cache();
        }
        let pixels = self.renderer.screenshot(&viewport, theme.background());
        (width, height, pixels)
    }

    /// Orden de foco de `app` (accesibilidad): los campos enfocables en el orden en que
    /// los recorre `Tab` (el de la interfaz, como hace la ventana con `focus_next`), con su
    /// identificador (`None` si no tiene), y el índice enfocado tras cada una de `presses`
    /// pulsaciones de `Tab` (o de `Mayús+Tab` con `backwards`) empezando sin foco.
    pub fn focus_order(&mut self, app: &App, presses: usize, backwards: bool) -> FocusOrder {
        let size = Size::new(SIZE.0 as f32, SIZE.1 as f32);
        let mut interface = UserInterface::build(
            app.view(),
            size,
            user_interface::Cache::new(),
            &mut self.renderer,
        );
        let mut recorder = FocusRecorder::default();
        interface.operate(&self.renderer, &mut recorder);
        let ids = recorder.stops.iter().map(|(id, _)| id.clone()).collect();
        let mut visited = Vec::new();
        for _ in 0..presses {
            super::focus::move_focus(&mut interface, &self.renderer, backwards);
            let mut recorder = FocusRecorder::default();
            interface.operate(&self.renderer, &mut recorder);
            visited.push(recorder.stops.iter().position(|(_, focused)| *focused));
        }
        FocusOrder { ids, visited }
    }
}

/// Resultado de [`ShotRenderer::focus_order`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusOrder {
    /// Identificador de cada campo enfocable, en el orden de `Tab`.
    pub ids: Vec<Option<String>>,
    /// Índice enfocado tras cada pulsación.
    pub visited: Vec<Option<usize>>,
}

/// Recorre los enfocables y anota su identificador y si tienen el foco.
#[derive(Default)]
struct FocusRecorder {
    stops: Vec<(Option<String>, bool)>,
}

impl Operation for FocusRecorder {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(
        &mut self,
        id: Option<&iced_core::widget::Id>,
        _bounds: iced_core::Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        // `Id` no expone su texto: se saca de `Debug` (`Id(Custom("room-code"))`).
        let name = id.and_then(|id| {
            let debug = format!("{id:?}");
            let start = debug.find('"')? + 1;
            let end = debug.rfind('"')?;
            (end > start).then(|| debug[start..end].to_owned())
        });
        self.stops.push((name, state.is_focused()));
    }
}

/// Todas las escenas, en claro y en oscuro.
pub fn scenes() -> Vec<Scene> {
    let fonts = Fonts::detect(Some("Inter 11"));
    let mut scenes = Vec::new();
    for (mode, suffix) in [(Mode::Light, "claro"), (Mode::Dark, "oscuro")] {
        for (name, build) in BUILDERS {
            let mut app = base(mode, fonts);
            build(&mut app);
            app.refresh_theme();
            scenes.push(Scene {
                name: format!("{name}-{suffix}"),
                app,
            });
        }
    }
    scenes
}

type Builder = fn(&mut App);

const BUILDERS: [(&str, Builder); 23] = [
    ("01-servidores", servers),
    ("02-servidor-nuevo", new_server),
    ("03-entrar", sign_in),
    ("04-crear-cuenta", create_account),
    ("05-inicio", home),
    ("06-inicio-vacio", home_empty),
    ("07-amigos", friends),
    ("08-compartir", host_setup),
    ("09-sala-abierta", host_room),
    ("10-dejar-de-compartir", host_stop_dialog),
    ("11-conectando", guest_connecting),
    ("12-estimacion", guest_estimate),
    ("13-video", guest_video),
    ("14-estadisticas", guest_stats),
    ("15-reconectando", guest_reconnecting),
    ("16-ajustes-video", |app| {
        settings_page(app, SettingsPage::Video)
    }),
    ("17-ajustes-audio", |app| {
        settings_page(app, SettingsPage::Audio)
    }),
    ("18-ajustes-entrada", |app| {
        settings_page(app, SettingsPage::Input)
    }),
    ("19-ajustes-red", |app| {
        settings_page(app, SettingsPage::Network)
    }),
    ("20-ajustes-dispositivos", |app| {
        settings_page(app, SettingsPage::Devices)
    }),
    ("21-ajustes-apariencia", |app| {
        settings_page(app, SettingsPage::Appearance)
    }),
    ("22-ajustes-acerca", |app| {
        settings_page(app, SettingsPage::About)
    }),
    ("23-diagnostico", |app| {
        settings_page(app, SettingsPage::Diagnostics)
    }),
];

fn server_entry() -> ServerEntry {
    ServerEntry {
        url: SERVER.to_owned(),
        fingerprint: None,
        tcp_port: None,
        username: Some("ana".to_owned()),
    }
}

fn base(mode: Mode, fonts: Fonts) -> App {
    let mut settings = Settings::default();
    settings.appearance.theme = match mode {
        Mode::Light => ThemeChoice::Light,
        Mode::Dark => ThemeChoice::Dark,
    };
    settings.servers = vec![
        server_entry(),
        ServerEntry {
            url: "https://juegos.example.org".to_owned(),
            fingerprint: Some("3fa1".repeat(16)),
            tcp_port: Some(8443),
            username: Some("ana".to_owned()),
        },
    ];
    settings.peers = vec![KnownPeer {
        name: "Bea".to_owned(),
        fingerprint: "b7".repeat(32),
        safety_number: safety().to_string(),
    }];
    let system = SystemAppearance {
        mode: Some(mode),
        accent: None,
        font: Some("Inter 11".to_owned()),
        source: "portal Settings",
    };
    let now = Instant::now();
    let mut app = App::new(settings, system, fonts, now);
    app.login = login::State::new(&app.settings);
    app
}

fn safety() -> SafetyNumber {
    SafetyNumber::between(&[7; 32], &[11; 32])
}

fn friend(id: u64, username: &str, display_name: &str, online: bool) -> Friend {
    Friend {
        id,
        username: username.to_owned(),
        display_name: display_name.to_owned(),
        online,
    }
}

/// Entra con la cuenta de Ana y deja la barra lateral en `page`.
fn signed_in(app: &mut App, page: Page) {
    app.update(Message::Event(Event::SignedIn(Ok(AccountInfo {
        server: server_entry(),
        username: "ana".to_owned(),
        display_name: "Ana".to_owned(),
        user_id: 1,
        device: "Portátil de Ana".to_owned(),
        fingerprint: "a1".repeat(32),
    }))));
    app.update(Message::Event(Event::Friends(Ok(Friends {
        friends: vec![
            friend(2, "bea", "Bea", true),
            friend(3, "carla", "Carla", true),
            friend(4, "dani", "Dani", false),
            friend(5, "eli", "Eli", false),
        ],
        incoming: vec![FriendRequest {
            id: 6,
            user: friend(6, "fran", "Fran", true),
            created_at: 1_760_000_000,
        }],
        outgoing: vec![FriendRequest {
            id: 7,
            user: friend(7, "gael", "Gael", false),
            created_at: 1_760_100_000,
        }],
    }))));
    app.update(Message::Event(Event::Usage(Ok(Usage {
        month: "2026-10".to_owned(),
        bytes: 18_400_000_000,
        quota_bytes: Some(50_000_000_000),
        relay_enabled: true,
        days: Vec::new(),
    }))));
    app.update(Message::Navigate(page));
    app.toasts.clear();
}

fn servers(app: &mut App) {
    app.screen = Screen::Login;
    app.login.step = Step::Servers;
}

fn probe_result() -> ServerProbe {
    ServerProbe {
        name: "Sala de juegos".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: coroom_transport::proto::PROTOCOL_VERSION,
        http3: true,
        udp_reachable: Some(true),
        fingerprint: "9c4e".repeat(16),
        untrusted: Some("certificado autofirmado".to_owned()),
    }
}

fn new_server(app: &mut App) {
    app.screen = Screen::Login;
    app.login.step = Step::AddServer;
    app.login.url = "https://salon.example.net".to_owned();
    app.login.probe = Probe::Done {
        url: app.login.url.clone(),
        probe: probe_result(),
    };
}

fn sign_in(app: &mut App) {
    app.screen = Screen::Login;
    app.login.step = Step::SignIn;
    app.login.server = Some(server_entry());
    app.login.username = "ana".to_owned();
    app.login.password = "change-me".to_owned();
    app.login.device = "Portátil de Ana".to_owned();
}

fn create_account(app: &mut App) {
    sign_in(app);
    app.login.invitation = true;
    app.login.code = "QX7P-2M4K".to_owned();
    app.login.display_name = "Ana".to_owned();
    app.login.password2 = "change".to_owned();
    app.login.errors.password2 = Some("Las contraseñas no coinciden".to_owned());
}

fn home(app: &mut App) {
    signed_in(app, Page::Home);
    app.invitations = vec![Invitation {
        from_user: 2,
        from_name: "Bea".to_owned(),
        code: "K7Q2MZ".to_owned(),
    }];
    // Aviso de versión nueva.
    app.update.available = Some(crate::update::Update {
        version: "0.2.0".to_owned(),
        url: format!("{}/tag/v0.2.0", crate::update::RELEASES_PAGE),
    });
}

fn home_empty(app: &mut App) {
    signed_in(app, Page::Home);
    app.friends = Load::Loaded(Friends {
        friends: Vec::new(),
        incoming: Vec::new(),
        outgoing: Vec::new(),
    });
    app.usage = None;
}

fn friends(app: &mut App) {
    signed_in(app, Page::Friends);
    app.friends_page.add_name = "hugo".to_owned();
}

fn host_setup(app: &mut App) {
    signed_in(app, Page::Host);
}

fn guest_status(id: u32, name: &str, permissions: Permissions, rtt: f64) -> GuestStatus {
    GuestStatus {
        guest: GuestId(id),
        user: u64::from(id) + 1,
        name: name.to_owned(),
        public_key: [id as u8; 32],
        fingerprint: format!("{:02x}", id + 0xb0).repeat(32),
        safety_number: safety(),
        permissions,
        leaf: id,
        path: Some(if id == 1 { Path::Direct } else { Path::Relayed }),
        rtt_ms: Some(rtt),
        target_kbps: Some(if id == 1 { 24_000 } else { 9_500 }),
        input_key: true,
        input: InputLatency::default(),
    }
}

fn host_room(app: &mut App) {
    signed_in(app, Page::Host);
    let preview = VideoFeed::detached();
    preview.push(game_frame(1, 640, 360), None, None);
    app.update(Message::Host(super::screens::host::Msg::Start));
    app.update(Message::Event(Event::HostStarted(Ok(preview))));
    app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
        code: "K7Q2MZ".to_owned(),
        fingerprint: "a1".repeat(32),
    })));
    app.update(Message::Event(Event::Host(SessionEvent::VideoCodec {
        codec: coroom_session::codec::Codec::Av1,
        detail: "codifica SVT-AV1".to_owned(),
    })));
    app.update(Message::Event(Event::HostGuests(vec![
        guest_status(
            1,
            "Bea",
            Permissions {
                keyboard_mouse: true,
                gamepad_slot: None,
                voice: true,
            },
            14.2,
        ),
        guest_status(
            2,
            "Carla",
            Permissions {
                keyboard_mouse: false,
                gamepad_slot: Some(1),
                voice: false,
            },
            38.6,
        ),
    ])));
    app.toasts.clear();
}

fn host_stop_dialog(app: &mut App) {
    host_room(app);
    app.dialog = Some(Dialog::StopSharing);
}

/// Entra en la sala de Bea y llega hasta `until`.
fn joined(app: &mut App, estimate: bool, frame: bool) {
    signed_in(app, Page::Home);
    app.update(Message::Home(super::screens::home::Msg::Code(
        "K7Q2MZ".to_owned(),
    )));
    app.update(Message::Home(super::screens::home::Msg::Join));
    let feed = VideoFeed::detached();
    if frame {
        feed.push(
            game_frame(2, 1280, 720),
            Some(cursor()),
            Some(cursor_image()),
        );
    }
    app.update(Message::Event(Event::GuestStarted(Ok(feed))));
    app.update(Message::Event(Event::Guest(SessionEvent::Joined {
        host_name: "Bea".to_owned(),
        host_fingerprint: "b7".repeat(32),
        safety_number: safety(),
    })));
    app.update(Message::Event(Event::Guest(SessionEvent::EpochChanged {
        epoch: 1,
    })));
    app.update(Message::Event(Event::Guest(SessionEvent::Connected {
        path: Path::Direct,
    })));
    if estimate {
        app.update(Message::Event(Event::Guest(
            SessionEvent::QualityEstimate {
                rtt_ms: 18.4,
                level: QualityLevel::from_rtt_ms(18.4),
            },
        )));
    }
    app.toasts.clear();
}

fn guest_connecting(app: &mut App) {
    joined(app, false, false);
    if let Some(state) = app.guest.as_mut() {
        state.keys = false;
        state.path = None;
    }
}

fn guest_estimate(app: &mut App) {
    joined(app, true, false);
}

/// Hasta el vídeo, con la barra visible.
fn video(app: &mut App) {
    joined(app, true, true);
    app.update(Message::Guest(guest::Msg::Continue));
    app.update(Message::Event(Event::Guest(SessionEvent::VideoCodec {
        codec: coroom_session::codec::Codec::Av1,
        detail: "decodifica Vulkan Video".to_owned(),
    })));
    app.update(Message::Event(Event::GuestStats(Box::new(sample_stats()))));
    app.update(Message::PointerMoved);
    app.toasts.clear();
    if let Some(state) = app.guest.as_mut() {
        state.phase = Phase::Video;
        state.bar_until = app.now + Duration::from_secs(3);
    }
}

fn guest_video(app: &mut App) {
    video(app);
}

fn guest_stats(app: &mut App) {
    video(app);
    app.update(Message::Guest(guest::Msg::ToggleStats));
}

fn guest_reconnecting(app: &mut App) {
    video(app);
    app.update(Message::Event(Event::Guest(SessionEvent::Reconnecting {
        attempt: 2,
        of: 5,
        retry_in: Duration::from_secs(2),
        reason: "la red dejó de responder".to_owned(),
    })));
    app.toasts.clear();
}

/// Segundos Unix de hace `ago`.
fn unix_ago(ago: Duration) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(now.saturating_sub(ago).as_secs()).unwrap_or_default()
}

fn settings_page(app: &mut App, page: SettingsPage) {
    signed_in(app, Page::Settings);
    let day = Duration::from_secs(86_400);
    app.settings_page.page = page;
    app.settings_page.devices = Load::Loaded(Devices {
        current: Some(1),
        devices: vec![
            Device {
                id: 1,
                name: "Portátil de Ana".to_owned(),
                fingerprint: "a1".repeat(32),
                created_at: unix_ago(day * 90),
                last_seen_at: unix_ago(Duration::from_secs(60)),
            },
            Device {
                id: 2,
                name: "Sobremesa del salón".to_owned(),
                fingerprint: "c4".repeat(32),
                created_at: unix_ago(day * 40),
                last_seen_at: unix_ago(day * 3),
            },
        ],
    });
    app.settings_page.audio_devices = Load::Loaded(vec![
        AudioDevice {
            name: "usb-mic".to_owned(),
            description: "Micrófono USB".to_owned(),
            input: true,
            default: true,
        },
        AudioDevice {
            name: "builtin-speakers".to_owned(),
            description: "Altavoces integrados".to_owned(),
            input: false,
            default: true,
        },
        AudioDevice {
            name: "headset".to_owned(),
            description: "Auriculares".to_owned(),
            input: false,
            default: false,
        },
    ]);
    app.settings_page.doctor = Load::Loaded(sample_doctor());
    // Lo que hace un equipo con NVENC con 4:4:4 y HDR y una comprobación de versión.
    use coroom_session::codec::Codec;
    app.video_support = Some(crate::ui::app::VideoSupport {
        yuv444: vec![
            (Codec::Hevc, "HEVC (NVENC)".to_owned()),
            (Codec::H264, "H.264 (NVENC)".to_owned()),
        ],
        hdr10: vec![
            (Codec::Av1, "AV1 (NVENC)".to_owned()),
            (Codec::Hevc, "HEVC (NVENC)".to_owned()),
        ],
        decode_yuv444: vec![Codec::Av1, Codec::Hevc],
        decode_hdr10: vec![Codec::Av1, Codec::Hevc],
        hdr_display: Some(true),
    });
    app.settings.video.chroma_444 = true;
    app.settings.video.fps = 120;
    app.settings.updates.last_check = Some(1_790_000_000);
}

fn sample_doctor() -> Vec<String> {
    let mut lines = vec![
        format!("coroom {}", env!("CARGO_PKG_VERSION")),
        "Sistema: Linux x86_64 (Wayland)".to_owned(),
        format!(
            "Versión del protocolo: {}",
            coroom_transport::proto::PROTOCOL_VERSION
        ),
    ];
    lines.extend(
        [
            "Backends de captura compilados: pipewire, sintética",
            "Captura de pantalla: portal ScreenCast disponible (PipeWire 1.4)",
            "Adaptador gráfico: GPU dedicada (Vulkan 1.4)",
            "Entrada del anfitrión:",
            "  teclado y ratón: libei por el portal RemoteDesktop",
            "  mandos virtuales: uinput disponible",
            "Audio:",
            "  PipeWire: captura de aplicaciones y del sistema",
            "  dispositivos: Micrófono USB (entrada), Altavoces integrados (salida)",
            "Códecs de vídeo (sondeo real de cada combinación):",
            "  H.265 Vulkan Video: codifica y decodifica",
            "  AV1 Vulkan Video: decodifica",
            "  H.264 openh264: codifica y decodifica (respaldo)",
            "Cero copias:",
            "  importa DMA-BUF (12 modificadores)",
            "  presentación: imágenes Vulkan del decodificador sin copia",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    lines
}

/// Estadísticas de ejemplo: un registro de latencia con 120 fotogramas plausibles y el enlace.
fn sample_stats() -> Stats {
    let ledger = LatencyLedger::new();
    ledger.set_video("AV1 1920×1080 a 60 fps (Vulkan Video)");
    let start = Instant::now();
    // Microsegundos por etapa (captura → codificado → … → presentado), con algo de variación.
    let steps = [2_600u64, 180, 40, 6_900, 60, 1_700, 3_100];
    for frame in 1..=120u64 {
        let jitter = (frame * 37) % 900;
        let mut at = start + Duration::from_micros(frame * 16_667);
        ledger.mark(frame, Stage::Captured, at);
        for (stage, step) in Stage::ALL[1..].iter().zip(steps) {
            let extra = if *stage == Stage::Received { jitter } else { 0 };
            at += Duration::from_micros(step + extra);
            ledger.mark(frame, *stage, at);
        }
        ledger.record_encoded_bytes_at(42_000, at);
    }
    let mut summary = ledger.summary();
    summary.fps = 60.0;
    summary.bitrate_kbps = 19_800.0;
    summary.link = Some(LinkSummary {
        path: "directo".to_owned(),
        rtt_ms: 13.8,
        target_kbps: 24_000,
        loss_percent: 0.3,
        groups_dropped: 2,
        groups_recovered: 41,
        epoch: 1,
        safety_number: safety().to_string(),
        peer_fingerprint: "b7".repeat(32),
        peer_name: "Bea".to_owned(),
        sframe: true,
    });
    summary.input = Some(InputLatency {
        samples: 840,
        p50_us: 9_200,
        p95_us: 14_100,
        p99_us: 18_000,
        max_us: 22_500,
        applied: 840,
        dropped: 0,
    });
    Stats {
        summary,
        allowed: coroom_session::input::capture::Allowed {
            keyboard_mouse: true,
            gamepad_slot: None,
        },
        mic_muted: false,
        game_muted: false,
        voice: true,
        audio_status: "micrófono activo".to_owned(),
        window_copies: 0,
    }
}

/// Un fotograma de juego de ejemplo (cielo, sol, colinas y suelo), en I420 BT.709 de rango
/// limitado como los del decodificador.
pub fn game_frame(frame_id: u64, width: u32, height: u32) -> PresentFrame {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    let pixel = |px: usize, py: usize| -> (f32, f32, f32) {
        let fx = px as f32 / w as f32;
        let fy = py as f32 / h as f32;
        let hill = 0.62 + 0.06 * (fx * 9.0).sin() + 0.03 * (fx * 23.0 + 1.3).sin();
        let far = 0.55 + 0.05 * (fx * 5.0 + 2.0).sin();
        let (sx, sy) = (fx - 0.72, (fy - 0.28) * h as f32 / w as f32);
        let sun = (sx * sx + sy * sy).sqrt();
        if fy > hill {
            let shade = 1.0 - (fy - hill) * 0.9;
            (0.20 * shade, 0.52 * shade, 0.24 * shade)
        } else if fy > far {
            (0.33, 0.47, 0.55)
        } else if sun < 0.06 {
            (1.0, 0.86, 0.45)
        } else {
            let t = fy / far;
            let glow = (0.25 - sun).max(0.0) * 1.6;
            (
                0.32 + 0.45 * t + glow,
                0.52 + 0.30 * t + glow * 0.7,
                0.86 - 0.10 * t + glow * 0.2,
            )
        }
    };
    let luma = |(r, g, b): (f32, f32, f32)| 16.0 + 46.559 * r + 156.629 * g + 15.812 * b;
    for py in 0..h {
        for px in 0..w {
            y[py * w + px] = luma(pixel(px, py)).clamp(16.0, 235.0) as u8;
        }
    }
    for cy in 0..ch {
        for cx in 0..cw {
            let (r, g, b) = pixel((cx * 2).min(w - 1), (cy * 2).min(h - 1));
            let (r, g, b) = (r.min(1.0), g.min(1.0), b.min(1.0));
            u[cy * cw + cx] =
                (128.0 - 25.664 * r - 86.336 * g + 112.0 * b).clamp(16.0, 240.0) as u8;
            v[cy * cw + cx] =
                (128.0 + 112.0 * r - 101.730 * g - 10.270 * b).clamp(16.0, 240.0) as u8;
        }
    }
    PresentFrame {
        frame_id,
        width,
        height,
        image: PresentImage::I420 {
            y,
            u,
            v,
            y_stride: width,
            uv_stride: cw as u32,
        },
        color: coroom_render::ColorInfo::SDR,
    }
}

fn cursor() -> CursorOverlay {
    CursorOverlay {
        x: 820,
        y: 430,
        visible: true,
    }
}

/// Flecha de 12×18 con borde.
fn cursor_image() -> Arc<CursorImage> {
    let (width, height) = (12u32, 18u32);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for py in 0..height {
        for px in 0..width {
            let inside = px <= py && px + py / 2 < 13;
            if !inside {
                continue;
            }
            let edge = px == 0 || px == py || px + py / 2 >= 12 || py == height - 1;
            let value = if edge { 0 } else { 255 };
            let at = ((py * width + px) * 4) as usize;
            rgba[at..at + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
    Arc::new(CursorImage {
        id: 1,
        width,
        height,
        hotspot_x: 0,
        hotspot_y: 0,
        rgba,
    })
}
