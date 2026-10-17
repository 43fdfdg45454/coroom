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
use coroom_transport::api::ServerProbe;
use coroom_transport::crypto::SafetyNumber;
use coroom_transport::proto::api::{Friend, FriendRequest, Friends, Usage};
use coroom_ui::font::Fonts;
use coroom_ui::theme::Mode;
use iced_core::widget::operation::{self, Operation};
use iced_core::{Event as UiEvent, Font, Pixels, Size, mouse, renderer, window};
use iced_runtime::user_interface::{self, UserInterface};
use iced_wgpu::graphics::{Shell, Viewport};
use iced_wgpu::{Engine, Renderer, wgpu};

use super::app::{AccountInfo, App, Event, Message, Page, Screen};
use super::screens::login::{self, Probe, Step};
use super::screens::{account, diagnostics, friends, guest, home, host, settings};
use super::settings::{KnownPeer, ServerEntry, Settings, ThemeChoice};
use super::system::SystemAppearance;

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

    /// El motor de `iced`, para pruebas que construyen la interfaz a mano cuadro a cuadro.
    pub fn renderer(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    /// Dibuja `app` a `scale` (1.0, o 2.0 para HiDPI); devuelve el ancho, el alto y los píxeles
    /// RGBA.
    /// El tamaño es el de la ventana de `app` ([`App::window`]; [`SIZE`] salvo en las escenas
    /// del riel estrecho).
    pub fn render(&mut self, app: &App, scale: f32) -> (u32, u32, Vec<u8>) {
        let width = (app.window.width * scale).round() as u32;
        let height = (app.window.height * scale).round() as u32;
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

    /// Pasa `events` a `element` (un solo *widget* o un trozo de pantalla, en `size` lógico y
    /// con `theme`), cada uno con el puntero en su punto, y lo dibuja al final: los mensajes que
    /// publicó y la captura (ancho, alto y RGBA). Para probar interacciones con el ratón y el
    /// teclado sin ventana.
    pub fn interact<M>(
        &mut self,
        element: coroom_ui::Element<'_, M>,
        size: (u32, u32),
        theme: &coroom_ui::Theme,
        events: &[(UiEvent, iced_core::Point)],
    ) -> (Vec<M>, (u32, u32, Vec<u8>)) {
        let logical = Size::new(size.0 as f32, size.1 as f32);
        let mut interface = UserInterface::build(
            element,
            logical,
            user_interface::Cache::new(),
            &mut self.renderer,
        );
        let mut messages = Vec::new();
        let mut clipboard = iced_core::clipboard::Null;
        let mut cursor = mouse::Cursor::Unavailable;
        for (event, point) in events {
            cursor = mouse::Cursor::Available(*point);
            let _ = interface.update(
                std::slice::from_ref(event),
                cursor,
                &mut self.renderer,
                &mut clipboard,
                &mut messages,
            );
        }
        interface.draw(
            &mut self.renderer,
            theme,
            &renderer::Style {
                text_color: theme.text_color(),
            },
            cursor,
        );
        let viewport = Viewport::with_physical_size(Size::new(size.0, size.1), 1.0);
        let pixels = self.renderer.screenshot(&viewport, theme.background());
        (messages, (size.0, size.1, pixels))
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
        for (name, build) in builders() {
            let mut app = base(mode, fonts);
            build(&mut app);
            // Las capturas muestran la página ya entrada (sin el desliz del cambio de destino).
            app.page_change = None;
            app.refresh_theme();
            scenes.push(Scene {
                name: format!("{name}-{suffix}"),
                app,
            });
        }
    }
    scenes
}

/// Monta una escena: deja la aplicación en el estado que se quiere ver.
pub(crate) type Builder = fn(&mut App);

/// Escenas de inicio de sesión (el resto, en el módulo `shots` de cada pantalla).
const LOGIN_BUILDERS: &[(&str, Builder)] = &[
    ("01-servidores", servers),
    ("01-servidores-cambiar", |app| {
        home::shots::home(app);
        app.update(Message::SwitchServer);
    }),
    ("02-servidor-nuevo", new_server),
    ("03-entrar", sign_in),
    ("04-crear-cuenta", create_account),
];

/// Ventana estrecha (sigue con el riel plegado, como [`SIZE`]).
pub const NARROW: (u32, u32) = (900, 780);

/// Ventana ancha (maximizada en una pantalla de 1366 px): el riel con iconos y nombres.
pub const WIDE: (u32, u32) = (1366, 780);

/// Escenas del riel con la sala abierta (el pie con el micrófono y las peticiones), ancho y
/// estrecho.
const RAIL_BUILDERS: &[(&str, Builder)] = &[
    ("09-riel-ancho-inicio", |app| {
        host::shots::host_room_join_request(app);
        app.window = iced_core::Size::new(WIDE.0 as f32, WIDE.1 as f32);
        app.update(Message::Navigate(Page::Home));
    }),
    ("09-riel-ancho-sala", |app| {
        host::shots::host_room_join_request(app);
        app.window = iced_core::Size::new(WIDE.0 as f32, WIDE.1 as f32);
    }),
    ("09-riel-estrecho-amigos", |app| {
        host::shots::host_room_join_request(app);
        app.window = iced_core::Size::new(NARROW.0 as f32, NARROW.1 as f32);
        app.update(Message::Navigate(Page::Friends));
    }),
];

/// Todas las escenas, en el orden de las pantallas.
fn builders() -> Vec<(&'static str, Builder)> {
    LOGIN_BUILDERS
        .iter()
        .chain(home::shots::BUILDERS)
        .chain(friends::shots::BUILDERS)
        .chain(account::shots::BUILDERS)
        .chain(host::shots::BUILDERS)
        .chain(guest::shots::BUILDERS)
        .chain(settings::shots::BUILDERS)
        .chain(diagnostics::shots::BUILDERS)
        .chain(RAIL_BUILDERS)
        .copied()
        .collect()
}

pub(crate) fn server_entry() -> ServerEntry {
    ServerEntry {
        url: SERVER.to_owned(),
        fingerprint: None,
        tcp_port: None,
        username: Some("ana".to_owned()),
    }
}

pub(crate) fn base(mode: Mode, fonts: Fonts) -> App {
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
        reduced_motion: None,
        animation_factor: None,
        source: "portal Settings",
    };
    let now = Instant::now();
    let mut app = App::new(settings, system, fonts, now);
    app.login = login::State::new(&app.settings);
    for (name, colors) in SAMPLE_AVATARS {
        if let Some(version) = sample_avatar(name) {
            app.avatars.insert(version, sample_avatar_image(colors));
        }
    }
    app
}

/// Quién tiene avatar en las capturas (el resto, sus iniciales) y sus colores, de arriba abajo.
const SAMPLE_AVATARS: [(&str, [[u8; 3]; 2]); 3] = [
    ("Ana", [[0x3d, 0x7e, 0xd8], [0x1c, 0x3f, 0x8a]]),
    ("Bea", [[0xe0, 0x8a, 0x3c], [0xa8, 0x3a, 0x2a]]),
    ("Dani", [[0x3c, 0xb0, 0x8a], [0x1d, 0x5e, 0x66]]),
];

/// La versión del avatar de ejemplo de `name`, si tiene.
pub(crate) fn sample_avatar(name: &str) -> Option<u64> {
    SAMPLE_AVATARS
        .iter()
        .position(|(sample, _)| *sample == name)
        .map(|index| 0x5a_0000 + index as u64)
}

/// Avatar de ejemplo dibujado aquí (nada de fotos de nadie): un busto claro sobre un degradado,
/// con los bordes suavizados.
fn sample_avatar_image([top, bottom]: [[u8; 3]; 2]) -> coroom_ui::avatar::AvatarImage {
    let side = coroom_transport::proto::api::AVATAR_SIDE_MAX;
    let center = side as f32 / 2.0;
    let image = image::RgbaImage::from_fn(side, side, |x, y| {
        let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
        let t = y / side as f32;
        let mix = |a: u8, b: u8| f32::from(a) * (1.0 - t) + f32::from(b) * t;
        let background = [0, 1, 2].map(|i| mix(top[i], bottom[i]));
        // La cabeza (un círculo) y los hombros (media elipse que sale por abajo).
        let head = 50.0 - ((x - center).powi(2) + (y - 104.0).powi(2)).sqrt();
        let shoulders =
            (1.0 - (((x - center) / 96.0).powi(2) + ((y - 262.0) / 92.0).powi(2)).sqrt()) * 92.0;
        let cover = head.max(shoulders).clamp(0.0, 1.0);
        let pixel = background.map(|channel| channel + (240.0 - channel) * cover * 0.85);
        image::Rgba([pixel[0] as u8, pixel[1] as u8, pixel[2] as u8, 255])
    });
    coroom_ui::avatar::AvatarImage::from_rgba_levels(
        crate::avatar::levels(&image)
            .into_iter()
            .map(|level| (level.width(), level.into_raw()))
            .collect(),
    )
    .expect("avatar de ejemplo")
}

pub(crate) fn safety() -> SafetyNumber {
    SafetyNumber::between(&[7; 32], &[11; 32])
}

pub(crate) fn friend(id: u64, username: &str, display_name: &str, online: bool) -> Friend {
    Friend {
        id,
        username: username.to_owned(),
        display_name: display_name.to_owned(),
        online,
        activity: Default::default(),
        avatar: sample_avatar(display_name),
    }
}

/// Entra con la cuenta de Ana y deja la barra lateral en `page`.
pub(crate) fn signed_in(app: &mut App, page: Page) {
    app.update(Message::Event(Event::SignedIn(Ok(AccountInfo {
        server: server_entry(),
        username: "ana".to_owned(),
        display_name: "Ana".to_owned(),
        user_id: 1,
        device: "Portátil de Ana".to_owned(),
        fingerprint: "a1".repeat(32),
        avatar: sample_avatar("Ana"),
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

pub(crate) fn servers(app: &mut App) {
    app.screen = Screen::Login;
    app.login.step = Step::Servers;
}

pub(crate) fn probe_result() -> ServerProbe {
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

pub(crate) fn new_server(app: &mut App) {
    app.screen = Screen::Login;
    app.login.step = Step::AddServer;
    app.login.url = "https://salon.example.net".to_owned();
    app.login.probe = Probe::Done {
        url: app.login.url.clone(),
        probe: probe_result(),
    };
}

pub(crate) fn sign_in(app: &mut App) {
    app.screen = Screen::Login;
    app.login.step = Step::SignIn;
    app.login.server = Some(server_entry());
    app.login.username = "ana".to_owned();
    app.login.password = "change-me".to_owned();
    app.login.device = "Portátil de Ana".to_owned();
}

pub(crate) fn create_account(app: &mut App) {
    sign_in(app);
    app.login.invitation = true;
    app.login.code = "QX7P-2M4K".to_owned();
    app.login.display_name = "Ana".to_owned();
    app.login.password2 = "change".to_owned();
    app.login.errors.password2 = Some("Las contraseñas no coinciden".to_owned());
}

pub(crate) fn playing(apps: &[(&str, &str)]) -> Vec<coroom_session::audio::apps::PlayingApp> {
    apps.iter()
        .map(|(key, name)| coroom_session::audio::apps::PlayingApp::new(key, name, Vec::new()))
        .collect()
}

/// Segundos Unix de hace `ago`.
pub(crate) fn unix_ago(ago: Duration) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(now.saturating_sub(ago).as_secs()).unwrap_or_default()
}

pub(crate) fn sample_doctor() -> Vec<String> {
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

pub(crate) fn cursor() -> CursorOverlay {
    CursorOverlay {
        x: 820,
        y: 430,
        visible: true,
    }
}

/// Flecha de 12×18 con borde.
pub(crate) fn cursor_image() -> Arc<CursorImage> {
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
