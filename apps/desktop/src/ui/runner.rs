//! La ventana: `winit` + `iced_winit` + `iced_wgpu` a bajo nivel, con un solo `wgpu::Device`.
//!
//! `iced` 0.14 de alto nivel (`iced::application`) crea su propio dispositivo `wgpu`; el vídeo sin
//! copias necesita el del presentador, abierto con las colas y extensiones de Vulkan Video
//! ([`coroom_render::wgpu::GpuContext`]). Por eso la ventana lleva su propio bucle (como el
//! ejemplo `integration` de `iced`): crea la superficie y el dispositivo, monta el
//! `iced_wgpu::Engine` sobre ellos, convierte los eventos de `winit` con `iced_winit::conversion`,
//! construye la interfaz con `iced_runtime::UserInterface` y la dibuja con el vídeo dentro de la
//! misma pasada. Tener la ventana de `winit` a mano da además:
//!
//! - **Pantalla completa** (`F11`, botón de la barra) con `Window::set_fullscreen`.
//! - **Modo inmersivo**: cursor capturado (`CursorGrabMode::Locked`, o `Confined` y recentrado) y
//!   oculto, con los deltas crudos de `DeviceEvent::MouseMotion`.
//! - **Eventos crudos para [`InputCapture`]**: con el vídeo delante, el teclado va al juego si hay
//!   permiso (salvo `F11`) y el ratón también cuando la interfaz no lo usa (un clic en la barra no
//!   llega al anfitrión; con el cursor capturado no llega nada a la interfaz).
//! - Atajos de la aplicación (`Ctrl+Q`, `Ctrl+,`, `F11`, `Esc`, `Tab` y `Mayús+Tab`), el
//!   portapapeles (`window_clipboard`) y los métodos de entrada (IME).
//! - **HDR**: con HDR en Ajustes y si la superficie lo admite
//!   ([`GpuContext::hdr_available`]), la superficie va en `Rgba16Float` con `ExtendedSrgbLinear`;
//!   la interfaz y el vídeo se dibujan en la textura intermedia de
//!   [`coroom_render::hdr::HdrOutput`] y una pasada final la lleva a la superficie. Cambiar el ajuste
//!   rehace la superficie y el motor de `iced` en caliente.
//! - **Modo inmediato**: la superficie se configura con el modo de menor latencia que ofrezca
//!   (inmediato, *mailbox* o FIFO, en ese orden), también a 120 fps.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use coroom_render::hdr::HdrOutput;
use coroom_render::wgpu::{GpuContext, HdrSurface, create_instance};
use coroom_session::input::capture::{Action, CaptureMode, InputCapture, ShortcutKeys};
use coroom_session::input::gamepad::{ConnectedPad, GamepadCapture, PadLink};
use coroom_session::input::keymap::keycode_from_name;
use coroom_session::latency::LatencySummary;
use coroom_transport::crypto::Identity;
use coroom_ui::font::Fonts;
use iced_core::clipboard::Kind;

use iced_core::{
    Color, Event as UiEvent, Font, InputMethod, Pixels, Size, mouse, renderer, window,
};
use iced_runtime::user_interface::{self, UserInterface};
use iced_wgpu::graphics::{Shell, Viewport};
use iced_wgpu::{Engine, Renderer, wgpu};
use iced_winit::{Clipboard, conversion};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{DeviceEvent, DeviceId, ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use super::app::{App, Effect, Message, Screen, Shortcut};
use super::backend::{Backend, GuestControls, Notify};
use super::compose::Compositor;
use super::screens::guest::{self, Phase};
use super::screens::settings as settings_screen;
use super::settings::{self, Settings, Shortcuts};
use super::system;
use crate::probe::{ProbeOptions, decode_output_for};
use crate::remote::JoinOptions;

/// Título de la ventana.
pub const TITLE: &str = "coroom";

/// Cada cuánto se sincronizan los permisos y el tamaño del fotograma con la entrada del invitado.
const INPUT_SYNC: Duration = Duration::from_millis(100);

/// Qué abre la ventana.
pub enum Launch {
    /// La aplicación (`coroom` sin argumentos).
    App,
    /// `coroom probe` con ventana: el vídeo local y sus estadísticas.
    Probe(ProbeOptions),
    /// `coroom join` con ventana: directamente la sala.
    Join {
        /// Identidad.
        identity: Box<Identity>,
        /// Opciones.
        options: Box<JoinOptions>,
    },
}

/// Lo que llega al bucle desde otros hilos.
#[derive(Debug)]
enum UserEvent {
    /// Un mensaje para la interfaz.
    Message(Box<Message>),
    /// Hay un fotograma nuevo que dibujar.
    Redraw,
}

/// Entrada del invitado en la ventana.
struct WindowInput {
    controls: GuestControls,
    capture: InputCapture,
    gamepads: Option<GamepadCapture>,
    /// Último estado de los mandos que vio la interfaz.
    pads: Option<(Vec<ConnectedPad>, bool)>,
    confined: bool,
    mode: CaptureMode,
}

struct Ready {
    window: Arc<Window>,
    gpu: GpuContext,
    /// La superficie, el vídeo y la textura de la interfaz: compone y entrega.
    compositor: Arc<Compositor>,
    /// Formato de la textura en la que dibuja `iced` (el del compositor).
    render_format: wgpu::TextureFormat,
    /// Lo que se pidió en Ajustes la última vez que se configuró la superficie.
    hdr_requested: bool,
    renderer: Renderer,
    viewport: Viewport,
    cache: user_interface::Cache,
    clipboard: Clipboard,
    cursor: mouse::Cursor,
    modifiers: ModifiersState,
    app: App,
    backend: Backend,
    input: Option<WindowInput>,
    resized: bool,
    redraw_at: Option<Instant>,
    next_sync: Instant,
    ime: Option<(iced_core::Rectangle, iced_core::input_method::Purpose)>,
    interaction: mouse::Interaction,
    /// La aplicación pidió salir (`Ctrl+Q` confirmado).
    quit: bool,
}

struct Runner {
    launch: Option<Launch>,
    proxy: EventLoopProxy<UserEvent>,
    ready: Option<Ready>,
    error: Option<String>,
    result: Option<Result<LatencySummary, String>>,
}

/// Abre la ventana y la ejecuta hasta que se cierre. Con [`Launch::Probe`] y [`Launch::Join`]
/// devuelve el resumen de latencia final.
pub fn run(launch: Launch) -> Result<Option<LatencySummary>, String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|err| {
            let detail = err.to_string();
            let cause = detail
                .rsplit_once(": ")
                .map_or(detail.as_str(), |(_, cause)| cause);
            t!(
                "runner-no-hay-pantalla-disponible-cause-prueba",
                cause = cause
            )
        })?;
    let mut runner = Runner {
        launch: Some(launch),
        proxy: event_loop.create_proxy(),
        ready: None,
        error: None,
        result: None,
    };
    event_loop
        .run_app(&mut runner)
        .map_err(|err| t!("runner-error-del-bucle-de-la-ventana", err = err))?;
    if let Some(mut ready) = runner.ready.take() {
        let result = ready.backend.shutdown();
        runner.result = runner.result.or(result);
    }
    if let Some(err) = runner.error {
        return Err(err);
    }
    runner.result.transpose()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Letra de un atajo como tecla de `winit`.
fn letter_key(letter: char) -> Option<KeyCode> {
    keycode_from_name(&letter.to_string())
}

fn shortcut_keys(shortcuts: &Shortcuts) -> ShortcutKeys {
    let default = ShortcutKeys::default();
    ShortcutKeys {
        immersive: letter_key(shortcuts.immersive).unwrap_or(default.immersive),
        overlay: letter_key(shortcuts.stats).unwrap_or(default.overlay),
        quit: letter_key(shortcuts.leave).unwrap_or(default.quit),
        mic: letter_key(shortcuts.mic).unwrap_or(default.mic),
        game: letter_key(shortcuts.game).unwrap_or(default.game),
        pads: letter_key(shortcuts.pads).unwrap_or(default.pads),
    }
}

/// Nombre de una tecla para Ajustes (`A`, `5`, `F12`, `CapsLock`).
pub fn key_name(code: KeyCode) -> String {
    let name = format!("{code:?}");
    if let Some(letter) = name.strip_prefix("Key") {
        return letter.to_owned();
    }
    if let Some(digit) = name.strip_prefix("Digit") {
        return digit.to_owned();
    }
    name
}

fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::SuperLeft
            | KeyCode::SuperRight
    )
}

/// Motor de `iced` sobre el dispositivo de la ventana, para vistas de `format`.
fn make_renderer(gpu: &GpuContext, format: wgpu::TextureFormat) -> Renderer {
    let engine = Engine::new(
        &gpu.adapter,
        gpu.device.clone(),
        gpu.queue.clone(),
        format,
        None,
        Shell::headless(),
    );
    Renderer::new(engine, Font::DEFAULT, Pixels(14.0))
}

/// Salida HDR para una superficie HDR (y el margen de la pantalla para el vídeo).
pub(super) fn make_hdr(
    gpu: &GpuContext,
    surface: Option<HdrSurface>,
) -> Option<(HdrOutput, HdrSurface)> {
    let surface = surface?;
    let output = HdrOutput::new(&gpu.device, coroom_render::hdr::HDR_FORMAT);
    output.set_scale(&gpu.queue, surface.sdr_white_scale);
    super::video::set_hdr_headroom(surface.headroom);
    tracing::info!(
        sdr_white_scale = surface.sdr_white_scale,
        headroom = surface.headroom,
        "superficie HDR (Rgba16Float, ExtendedSrgbLinear)"
    );
    Some((output, surface))
}

impl Runner {
    fn open(&mut self, event_loop: &ActiveEventLoop) -> Result<Ready, String> {
        let attributes = system::window_attributes(
            Window::default_attributes()
                .with_title(TITLE)
                .with_inner_size(LogicalSize::new(1180.0, 780.0))
                .with_min_inner_size(LogicalSize::new(760.0, 540.0)),
        );
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|err| t!("runner-no-se-pudo-abrir-la-ventana", err = err))?,
        );
        let instance = create_instance(Some(Box::new(event_loop.owned_display_handle())));
        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|err| t!("runner-no-se-pudo-crear-la-superficie", err = err))?;
        let gpu = GpuContext::new(&instance, Some(&surface))
            .map_err(|err| t!("runner-no-se-pudo-preparar-la-gpu", err = err))?;
        let size = window.inner_size();
        let settings_path = settings::path_in(&settings::data_dir());
        let (settings, warning) = Settings::load(&settings_path);
        let hdr_requested = settings.video.hdr;
        let setup = gpu
            .surface_setup_with(&surface, size.width, size.height, hdr_requested)
            .map_err(|err| t!("runner-no-se-pudo-preparar-la-ventana", err = err))?;
        surface.configure(&gpu.device, &setup.config);
        let hdr = make_hdr(&gpu, setup.hdr);
        let hdr_display = gpu.hdr_available(&surface);
        let compositor = Compositor::new(&gpu, surface, &setup, hdr);

        let appearance = system::read();
        let fonts = Fonts::detect(appearance.font.as_deref());
        let renderer = make_renderer(&gpu, setup.render_format);
        let viewport = Viewport::with_physical_size(
            Size::new(size.width, size.height),
            window.scale_factor() as f32,
        );

        let proxy = Arc::new(Mutex::new(self.proxy.clone()));
        let message_proxy = Arc::clone(&proxy);
        let notify: Notify = Arc::new(move |message| {
            let _ = lock(&message_proxy).send_event(UserEvent::Message(Box::new(message)));
        });
        let wake_proxy = Arc::clone(&proxy);
        let wake: super::video::Wake = Arc::new(move || {
            let _ = lock(&wake_proxy).send_event(UserEvent::Redraw);
        });
        let watch_notify = Arc::clone(&notify);
        system::watch(Box::new(move |appearance| {
            watch_notify(Message::System(appearance));
        }));

        let mut app = App::new(settings, appearance, fonts, Instant::now());
        if let Some(warning) = warning {
            app.toast(coroom_ui::theme::Tone::Warning, warning);
        }
        let mut backend = Backend::new(
            notify,
            wake,
            gpu.importer(),
            decode_output_for(gpu.vulkan_share()),
            settings_path,
            Some(Arc::clone(&compositor)),
        );
        backend.set_hdr_display(hdr_display);
        match self.launch.take().unwrap_or(Launch::App) {
            Launch::App => {
                app.login.restoring = !app.settings.servers.is_empty();
                backend.execute(Effect::Restore, &app);
                backend.execute(Effect::LoadVideoSupport, &app);
                let updates = &app.settings.updates;
                let due = crate::update::due(
                    updates.last_check,
                    crate::update::unix_now(),
                    updates.interval_hours,
                );
                app.schedule_update_check(due);
            }
            Launch::Probe(options) => {
                app.probe = true;
                app.screen = Screen::Guest;
                let feed = backend.start_probe(options);
                let mut state = guest::State::new("local".to_owned(), feed, app.now);
                state.probe = true;
                state.host_name = Some(t!("runner-este-equipo").to_owned());
                state.keys = true;
                app.guest = Some(state);
            }
            Launch::Join { identity, options } => {
                app.probe = true;
                app.screen = Screen::Guest;
                app.guest = Some(guest::State::new(
                    options.code.clone(),
                    super::video::VideoFeed::detached(),
                    app.now,
                ));
                backend.start_join(Some(*identity), *options);
            }
        }
        let clipboard = Clipboard::connect(Arc::clone(&window));
        Ok(Ready {
            window,
            compositor,
            render_format: setup.render_format,
            hdr_requested,
            gpu,
            renderer,
            viewport,
            cache: user_interface::Cache::new(),
            clipboard,
            cursor: mouse::Cursor::Unavailable,
            modifiers: ModifiersState::default(),
            app,
            backend,
            input: None,
            resized: false,
            redraw_at: None,
            next_sync: Instant::now(),
            ime: None,
            interaction: mouse::Interaction::None,
            quit: false,
        })
    }
}

impl Ready {
    /// Pasa eventos por la interfaz; devuelve qué hizo con cada uno.
    fn process(&mut self, events: &[UiEvent]) -> Vec<iced_core::event::Status> {
        let mut messages = Vec::new();
        let statuses = {
            let mut interface = UserInterface::build(
                self.app.view(),
                self.viewport.logical_size(),
                std::mem::take(&mut self.cache),
                &mut self.renderer,
            );
            let (_, statuses) = interface.update(
                events,
                self.cursor,
                &mut self.renderer,
                &mut self.clipboard,
                &mut messages,
            );
            self.cache = interface.into_cache();
            statuses
        };
        for message in messages {
            self.dispatch(message);
        }
        self.window.request_redraw();
        statuses
    }

    /// Reduce un mensaje y hace sus efectos.
    fn dispatch(&mut self, message: Message) {
        let attach = matches!(
            &message,
            Message::Event(super::app::Event::GuestStarted(Ok(_)))
        );
        let effects = self.app.update(message);
        if attach {
            self.attach_input();
        }
        self.run_effects(effects);
        if self.app.guest.is_none() && self.input.is_some() {
            self.detach_input();
        }
    }

    fn run_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Copy(text) => self.clipboard.write(Kind::Standard, text),
                Effect::SetFullscreen(on) => {
                    self.window
                        .set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
                    let more = self.app.update(Message::FullscreenChanged(on));
                    self.run_effects(more);
                }
                Effect::Quit => self.quit = true,
                Effect::SetImmersive(on) => {
                    if let Some(input) = self.input.as_mut() {
                        let actions = input.capture.set_immersive(on);
                        self.apply(actions);
                    }
                }
                Effect::SetPadChoice(choice) => {
                    if let Some(gamepads) = self.input.as_ref().and_then(|i| i.gamepads.as_ref()) {
                        gamepads.set_choice(choice);
                    }
                }
                Effect::SaveSettings => {
                    if let Some(input) = self.input.as_mut() {
                        input
                            .capture
                            .set_shortcuts(shortcut_keys(&self.app.settings.input.shortcuts));
                    }
                    self.backend.execute(Effect::SaveSettings, &self.app);
                }
                other => self.backend.execute(other, &self.app),
            }
        }
    }

    fn attach_input(&mut self) {
        let Some(controls) = self.backend.guest_controls() else {
            return;
        };
        let size = self.window.inner_size();
        let mut capture = InputCapture::new();
        capture.set_window_size(size.width, size.height);
        capture.set_shortcuts(shortcut_keys(&self.app.settings.input.shortcuts));
        if let Some(name) = &controls.push_to_talk {
            capture.set_push_to_talk(keycode_from_name(name));
        }
        let link: Arc<dyn PadLink> = Arc::new(controls.input.clone());
        let gamepads = GamepadCapture::spawn(link, self.app.settings.input.pad.clone()).ok();
        self.input = Some(WindowInput {
            controls,
            capture,
            gamepads,
            pads: None,
            confined: false,
            mode: CaptureMode::Desktop,
        });
    }

    fn detach_input(&mut self) {
        if let Some(mut input) = self.input.take() {
            input.gamepads.take();
            let _ = self.window.set_cursor_grab(CursorGrabMode::None);
            self.window.set_cursor_visible(true);
        }
    }

    /// Aplica lo que pide la captura de entrada.
    fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send(event) => {
                    if let Some(input) = &self.input {
                        input.controls.input.send(event);
                    }
                }
                Action::Grab(grab) => self.grab(grab),
                Action::ToggleOverlay => {
                    self.dispatch(Message::Guest(guest::Msg::ToggleStats));
                }
                Action::TogglePads => {
                    self.dispatch(Message::Guest(guest::Msg::TogglePads));
                }
                Action::Quit => {
                    let effects = guest::leave(&mut self.app);
                    self.run_effects(effects);
                }
                Action::ToggleMicMute => {
                    if let Some(input) = &self.input {
                        input.controls.audio.toggle_mic();
                    }
                }
                Action::ToggleGameMute => {
                    if let Some(input) = &self.input {
                        input.controls.audio.toggle_game();
                    }
                }
                Action::PushToTalk(pressed) => {
                    if let Some(input) = &self.input {
                        input.controls.audio.set_talk_pressed(pressed);
                    }
                }
            }
        }
        let mode = self.input.as_ref().map(|input| input.capture.mode());
        if let (Some(mode), Some(input)) = (mode, self.input.as_mut())
            && mode != input.mode
        {
            input.mode = mode;
            self.dispatch(Message::Guest(guest::Msg::ImmersiveChanged(
                mode == CaptureMode::Immersive,
            )));
        }
    }

    /// Captura (bloqueado, o confinado si el compositor no admite bloquear) u oculta el cursor.
    fn grab(&mut self, grab: bool) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        if !grab {
            let _ = self.window.set_cursor_grab(CursorGrabMode::None);
            self.window.set_cursor_visible(true);
            input.confined = false;
            return;
        }
        match self.window.set_cursor_grab(CursorGrabMode::Locked) {
            Ok(()) => input.confined = false,
            Err(_) => match self.window.set_cursor_grab(CursorGrabMode::Confined) {
                Ok(()) => input.confined = true,
                Err(err) => {
                    input.capture.grab_failed();
                    self.app.toast(
                        coroom_ui::theme::Tone::Warning,
                        t!("runner-no-se-pudo-capturar-el-cursor", err = err),
                    );
                    return;
                }
            },
        }
        self.window.set_cursor_visible(false);
    }

    /// Permisos y tamaño del fotograma que dijo el anfitrión, a la captura.
    fn sync_input(&mut self) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        if let Some((width, height)) = input.controls.input.host_frame_size() {
            input.capture.set_host_frame(width, height);
        }
        let allowed = input.controls.input.allowed();
        let mut pads = None;
        if let Some(gamepads) = &input.gamepads {
            let now = (
                gamepads.pads(),
                allowed
                    .gamepad_slot
                    .is_some_and(|slot| input.controls.input.raw(slot)),
            );
            if input.pads.as_ref() != Some(&now) {
                input.pads = Some(now.clone());
                pads = Some(now);
            }
        }
        if allowed != input.capture.allowed() {
            if let Some(gamepads) = &input.gamepads {
                gamepads.set_slot(allowed.gamepad_slot);
            }
            let actions = input.capture.set_allowed(allowed);
            self.apply(actions);
        }
        if let Some((pads, raw)) = pads {
            self.dispatch(Message::Guest(guest::Msg::Pads(pads, raw)));
        }
    }

    /// Si el vídeo del invitado está delante (y nada lo tapa).
    fn video_in_front(&self) -> bool {
        self.input.is_some()
            && self.app.screen == Screen::Guest
            && self.app.dialog.is_none()
            && self
                .app
                .guest
                .as_ref()
                .is_some_and(|guest| guest.phase == Phase::Video)
    }

    fn shortcut(&mut self, code: KeyCode) -> bool {
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let shortcut = match code {
            KeyCode::KeyQ if ctrl => Shortcut::Quit,
            KeyCode::Comma if ctrl => Shortcut::Settings,
            KeyCode::F11 => Shortcut::Fullscreen,
            KeyCode::Escape => Shortcut::Escape,
            KeyCode::Tab => {
                let mut interface = UserInterface::build(
                    self.app.view(),
                    self.viewport.logical_size(),
                    std::mem::take(&mut self.cache),
                    &mut self.renderer,
                );
                super::focus::move_focus(&mut interface, &self.renderer, shift);
                self.cache = interface.into_cache();
                self.window.request_redraw();
                return true;
            }
            _ => return false,
        };
        self.dispatch(Message::Shortcut(shortcut));
        true
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match &event {
            WindowEvent::RedrawRequested => {
                self.redraw(event_loop);
                return;
            }
            WindowEvent::CloseRequested => {
                if self.app.host.is_open() && !self.app.probe {
                    self.dispatch(Message::Shortcut(Shortcut::Quit));
                } else {
                    event_loop.exit();
                }
                return;
            }
            WindowEvent::Resized(size) => {
                self.resized = true;
                if let Some(input) = self.input.as_mut() {
                    input.capture.set_window_size(size.width, size.height);
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => self.resized = true,
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = mouse::Cursor::Available(conversion::cursor_position(
                    *position,
                    self.viewport.scale_factor(),
                ));
                if !self
                    .input
                    .as_ref()
                    .is_some_and(|input| input.capture.grabbed())
                {
                    self.app.now = Instant::now();
                    let effects = self.app.update(Message::PointerMoved);
                    self.run_effects(effects);
                }
            }
            WindowEvent::CursorLeft { .. } => self.cursor = mouse::Cursor::Unavailable,
            WindowEvent::ThemeChanged(theme) => {
                let mut appearance = system::read();
                if appearance.mode.is_none() {
                    appearance.mode = Some(match theme {
                        winit::window::Theme::Dark => coroom_ui::theme::Mode::Dark,
                        winit::window::Theme::Light => coroom_ui::theme::Mode::Light,
                    });
                }
                self.dispatch(Message::System(appearance));
            }
            _ => {}
        }

        // Cambiar un atajo en Ajustes: la próxima tecla es para eso.
        if let WindowEvent::KeyboardInput { event: key, .. } = &event
            && self.app.settings_page.capturing.is_some()
            && key.state == ElementState::Pressed
            && let PhysicalKey::Code(code) = key.physical_key
            && !is_modifier(code)
        {
            let msg = if code == KeyCode::Escape {
                settings_screen::Msg::CancelCapture
            } else {
                settings_screen::Msg::KeyCaptured(key_name(code))
            };
            self.dispatch(Message::Settings(msg));
            return;
        }

        let in_front = self.video_in_front();
        let grabbed = self
            .input
            .as_ref()
            .is_some_and(|input| input.capture.grabbed());
        let keyboard_allowed = self
            .input
            .as_ref()
            .is_some_and(|input| input.capture.allowed().keyboard_mouse);
        let mut to_interface = true;
        let mut capture_now = false;
        let mut capture_if_ignored = false;
        match &event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                let code = match key.physical_key {
                    PhysicalKey::Code(code) => Some(code),
                    PhysicalKey::Unidentified(_) => None,
                };
                let pressed = key.state == ElementState::Pressed;
                if in_front {
                    to_interface = false;
                    if code == Some(KeyCode::F11) {
                        if pressed && !key.repeat {
                            self.shortcut(KeyCode::F11);
                        }
                    } else if code == Some(KeyCode::Escape) && !keyboard_allowed {
                        if pressed && !key.repeat {
                            self.shortcut(KeyCode::Escape);
                        }
                    } else {
                        capture_now = true;
                    }
                } else if pressed
                    && !key.repeat
                    && let Some(code) = code
                    && self.shortcut(code)
                {
                    return;
                }
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => {
                if in_front && grabbed {
                    to_interface = false;
                    capture_now = true;
                } else if in_front {
                    capture_if_ignored = true;
                }
            }
            WindowEvent::Focused(_) | WindowEvent::Resized(_) => {
                capture_now = self.input.is_some();
            }
            _ => {}
        }
        if capture_now && let Some(input) = self.input.as_mut() {
            let actions = input.capture.on_window_event(&event);
            self.apply(actions);
        }
        if !to_interface {
            return;
        }
        let converted = conversion::window_event(
            event.clone(),
            self.window.scale_factor() as f32,
            self.modifiers,
        );
        let Some(converted) = converted else {
            return;
        };
        let statuses = self.process(std::slice::from_ref(&converted));
        let ignored = statuses
            .first()
            .is_none_or(|status| *status == iced_core::event::Status::Ignored);
        // El cursor y los clics sobre el vídeo (no sobre la barra) van al anfitrión.
        if capture_if_ignored
            && (ignored || matches!(event, WindowEvent::CursorMoved { .. }))
            && let Some(input) = self.input.as_mut()
        {
            let actions = input.capture.on_window_event(&event);
            self.apply(actions);
        }
    }

    fn reconfigure(&mut self) {
        let size = self.window.inner_size();
        self.viewport = Viewport::with_physical_size(
            Size::new(size.width.max(1), size.height.max(1)),
            self.window.scale_factor() as f32,
        );
        self.compositor.resize(size.width, size.height);
        self.resized = false;
    }

    /// Rehace la superficie y el motor de `iced` si cambió el ajuste de HDR.
    fn apply_hdr(&mut self) {
        let wanted = self.app.settings.video.hdr;
        if wanted == self.hdr_requested {
            return;
        }
        self.hdr_requested = wanted;
        let size = self.window.inner_size();
        let setup = match self
            .compositor
            .configure_hdr(&self.gpu, size.width, size.height, wanted)
        {
            Ok(setup) => setup,
            Err(err) => {
                tracing::warn!(error = %err, "no se pudo cambiar la superficie");
                return;
            }
        };
        if wanted && setup.hdr.is_none() {
            tracing::info!("la pantalla no expone HDR: el vídeo HDR se ve con tone mapping");
        }
        if setup.render_format != self.render_format {
            self.render_format = setup.render_format;
            self.renderer = make_renderer(&self.gpu, self.render_format);
            self.cache = user_interface::Cache::new();
        }
        self.window.request_redraw();
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        self.apply_hdr();
        if self.resized {
            self.reconfigure();
        }
        let now = Instant::now();
        self.app.now = now;
        let effects = self.app.update(Message::Tick);
        self.run_effects(effects);
        if self.should_exit() {
            event_loop.exit();
            return;
        }
        let mut messages = Vec::new();
        let mut ime = None;
        let theme = self.app.theme;
        let mut interface = UserInterface::build(
            self.app.view(),
            self.viewport.logical_size(),
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let (state, _) = interface.update(
            &[UiEvent::Window(window::Event::RedrawRequested(now))],
            self.cursor,
            &mut self.renderer,
            &mut self.clipboard,
            &mut messages,
        );
        if let user_interface::State::Updated {
            mouse_interaction,
            redraw_request,
            input_method,
            ..
        } = state
        {
            let grabbed = self
                .input
                .as_ref()
                .is_some_and(|input| input.capture.grabbed());
            if !grabbed && mouse_interaction != self.interaction {
                match conversion::mouse_interaction(mouse_interaction) {
                    Some(icon) => {
                        self.window.set_cursor(icon);
                        self.window.set_cursor_visible(true);
                    }
                    None => self.window.set_cursor_visible(false),
                }
                self.interaction = mouse_interaction;
            }
            match redraw_request {
                window::RedrawRequest::NextFrame => self.window.request_redraw(),
                window::RedrawRequest::At(at) => {
                    self.redraw_at = Some(self.redraw_at.map_or(at, |old| old.min(at)));
                }
                window::RedrawRequest::Wait => {}
            }
            ime = Some(input_method);
        }
        interface.draw(
            &mut self.renderer,
            &theme,
            &renderer::Style {
                text_color: theme.text_color(),
            },
            self.cursor,
        );
        self.cache = interface.into_cache();
        if let Some(method) = ime {
            self.input_method(method);
        }
        // La interfaz va a su textura (transparente donde no dibuja nada: el vídeo se ve
        // debajo) y el compositor la pone sobre el fondo y el vídeo y entrega.
        let format = self.render_format;
        let renderer = &mut self.renderer;
        let viewport = &self.viewport;
        self.compositor.draw_ui(self.app.backdrop(), |target| {
            let _ = renderer.present(Some(Color::TRANSPARENT), format, target, viewport);
        });
        for message in messages {
            self.dispatch(message);
        }
        if self.app.animating() {
            self.window.request_redraw();
        }
    }

    /// Si hay que cerrar: salir confirmado o, en `probe`/`join` con ventana, la sala terminó.
    fn should_exit(&self) -> bool {
        self.quit || (self.app.probe && self.app.screen != Screen::Guest)
    }

    fn input_method(&mut self, method: InputMethod) {
        match method {
            InputMethod::Disabled => {
                if self.ime.take().is_some() {
                    self.window.set_ime_allowed(false);
                }
            }
            InputMethod::Enabled {
                cursor, purpose, ..
            } => {
                if self.ime.is_none() {
                    self.window.set_ime_allowed(true);
                }
                if self.ime != Some((cursor, purpose)) {
                    self.window.set_ime_cursor_area(
                        LogicalPosition::new(cursor.x, cursor.y),
                        LogicalSize::new(cursor.width, cursor.height),
                    );
                    self.window
                        .set_ime_purpose(conversion::ime_purpose(purpose));
                    self.ime = Some((cursor, purpose));
                }
            }
        }
    }

    /// Cuándo hay que volver a despertar sin eventos.
    fn next_wake(&self) -> Option<Instant> {
        let mut next = self.app.next_tick();
        let mut consider = |at: Option<Instant>| {
            if let Some(at) = at {
                next = Some(next.map_or(at, |next: Instant| next.min(at)));
            }
        };
        consider(self.redraw_at);
        if self.input.is_some() {
            consider(Some(self.next_sync));
        }
        next
    }
}

impl ApplicationHandler<UserEvent> for Runner {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        let Some(ready) = self.ready.as_mut() else {
            return;
        };
        if let StartCause::ResumeTimeReached { .. } = cause {
            let now = Instant::now();
            if ready.redraw_at.is_some_and(|at| at <= now) {
                ready.redraw_at = None;
            }
            if ready.input.is_some() && now >= ready.next_sync {
                ready.next_sync = now + INPUT_SYNC;
                ready.sync_input();
            }
            ready.window.request_redraw();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.ready.is_some() {
            return;
        }
        match self.open(event_loop) {
            Ok(ready) => {
                ready.window.request_redraw();
                self.ready = Some(ready);
            }
            Err(err) => {
                self.error = Some(err);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(ready) = self.ready.as_mut() else {
            return;
        };
        match event {
            UserEvent::Redraw => ready.window.request_redraw(),
            UserEvent::Message(message) => {
                ready.app.now = Instant::now();
                ready.dispatch(*message);
                ready.window.request_redraw();
            }
        }
        if ready.should_exit() {
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(ready) = self.ready.as_mut() else {
            return;
        };
        ready.window_event(event_loop, event);
        if ready.should_exit() {
            event_loop.exit();
        }
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _device: DeviceId,
        event: DeviceEvent,
    ) {
        let _ = event_loop;
        let DeviceEvent::MouseMotion { delta } = event else {
            return;
        };
        let Some(ready) = self.ready.as_mut() else {
            return;
        };
        let Some(input) = ready.input.as_mut() else {
            return;
        };
        let actions = input.capture.on_mouse_motion(delta.0, delta.1);
        if input.confined && !actions.is_empty() {
            // Confinado y no bloqueado: el cursor (oculto) vuelve al centro para no topar con el
            // borde; los deltas crudos no dependen de él.
            let size = ready.window.inner_size();
            let _ = ready.window.set_cursor_position(PhysicalPosition::new(
                f64::from(size.width) / 2.0,
                f64::from(size.height) / 2.0,
            ));
        }
        ready.apply(actions);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(ready) = self.ready.as_ref() else {
            return;
        };
        match ready.next_wake() {
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(ready) = self.ready.as_mut() {
            ready.detach_input();
            self.result = ready.backend.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_have_short_names() {
        assert_eq!(key_name(KeyCode::KeyA), "A");
        assert_eq!(key_name(KeyCode::Digit5), "5");
        assert_eq!(key_name(KeyCode::F12), "F12");
        assert_eq!(key_name(KeyCode::CapsLock), "CapsLock");
        assert!(keycode_from_name(&key_name(KeyCode::CapsLock)).is_some());
        let keys = shortcut_keys(&Shortcuts {
            immersive: 'K',
            ..Shortcuts::default()
        });
        assert_eq!(keys.immersive, KeyCode::KeyK);
        assert_eq!(keys.overlay, KeyCode::KeyO);
    }
}
