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
//!   llega al anfitrión; con el cursor capturado no llega nada a la interfaz), desde que el
//!   invitado hace clic en la imagen (clic para capturar, como QEMU) hasta que lo suelta con
//!   `Ctrl+Alt+R` o pierde el foco; suelto, `Esc` vuelve a ser de la aplicación. En modo escritorio
//!   la posición del cursor sale hacia el anfitrión **antes** de pasar por la interfaz.
//! - **Un dibujo por refresco**: los eventos de la ventana se convierten y se acumulan
//!   ([`Ready::queue`]); la interfaz los recibe todos de golpe en el siguiente `RedrawRequested`
//!   (como hace `iced_winit`), no se reconstruye por cada evento (un ratón a 1 kHz la
//!   reconstruiría mil veces por segundo). Solo un clic o la rueda con el vídeo delante pasan al
//!   momento, porque hay que saber si la interfaz los usó antes de dárselos al anfitrión. Con el
//!   vídeo delante, mover el cursor solo pide dibujar si está sobre algo de la interfaz
//!   ([`guest::pointer_over_ui`]).
//! - Atajos de la aplicación (`Ctrl+Q`, `Ctrl+,`, `F11`, `Esc`, `Tab` y `Mayús+Tab`), el
//!   portapapeles (`window_clipboard`) y los métodos de entrada (IME).
//! - **Chat sobre el vídeo**: mientras su campo tiene el foco, el teclado es de la interfaz y no
//!   del juego (las teclas pulsadas se sueltan en el anfitrión al enfocarlo); `Esc` devuelve el
//!   teclado al juego sin cerrar el panel y los atajos `Ctrl+Alt` siguen funcionando.
//! - **Sincronización vertical** opcional (Ajustes del invitado o panel de Conexión): `Fifo`.
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

use iced_core::event::Status;
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
use super::compose::{Compositor, Present};
use super::pacing::{self, FrameClock, NextFrame};
use super::screens::chat;
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
    /// Si reenviaba el teclado y el ratón la última vez que lo vio la interfaz.
    forwarding: bool,
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
    /// Sincronización vertical que tiene ahora el compositor.
    vsync_applied: bool,
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
    /// Eventos ya convertidos que la interfaz recibe de golpe en el próximo dibujo.
    pending: Vec<UiEvent>,
    /// Mientras se mira o se comparte una transmisión, el escritorio no se bloquea
    /// ([`App::wants_idle_inhibit`]).
    idle_inhibit: Option<crate::inhibit::IdleInhibitor>,
    /// Quién marca el ritmo de los cuadros de animación ([`pacing`]).
    clock: FrameClock,
    /// Cuándo empezó el último dibujo.
    last_frame: Instant,
    /// Si la interfaz quedó esperando al vídeo ([`Present::WithVideo`]) y el vídeo no llegó para
    /// entonces, se dibuja sola.
    video_fallback: Option<Instant>,
    /// Algún componente pidió el siguiente cuadro para su animación en este dibujo.
    ui_frame: bool,
}

/// Lo que devolvió una pasada de la interfaz ([`Ready::pass`]).
struct Pass {
    messages: Vec<Message>,
    /// El campo del chat tiene el foco.
    typing: bool,
    /// Qué quiere la interfaz de la ventana (si llegó a actualizarse).
    state: Option<(mouse::Interaction, window::RedrawRequest, InputMethod)>,
    /// Qué hizo con cada evento.
    statuses: Vec<Status>,
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
        chat: letter_key(shortcuts.chat).unwrap_or(default.chat),
        quit: letter_key(shortcuts.leave).unwrap_or(default.quit),
        mic: letter_key(shortcuts.mic).unwrap_or(default.mic),
        game: letter_key(shortcuts.game).unwrap_or(default.game),
        pads: letter_key(shortcuts.pads).unwrap_or(default.pads),
        release: letter_key(shortcuts.release).unwrap_or(default.release),
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

/// Si el campo del chat tiene el foco en `interface`.
fn chat_focused<M, T, R: iced_core::Renderer>(
    interface: &mut UserInterface<'_, M, T, R>,
    renderer: &R,
) -> bool {
    use iced_core::widget::operation::{self, Operation, Outcome, focusable};
    let mut op = focusable::is_focused(iced_core::widget::Id::new(chat::INPUT_ID));
    interface.operate(renderer, &mut operation::black_box(&mut op));
    matches!(op.finish(), Outcome::Some(true))
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
        app.host.restore_tokens = crate::portal::load_restore_tokens();
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
        let clock = frame_clock(&window);
        Ok(Ready {
            window,
            compositor,
            render_format: setup.render_format,
            hdr_requested,
            vsync_applied: false,
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
            pending: Vec::new(),
            idle_inhibit: None,
            clock,
            last_frame: Instant::now(),
            video_fallback: None,
            ui_frame: false,
        })
    }
}

impl Ready {
    /// Deja `event` para la interfaz en el próximo dibujo. Los movimientos del cursor seguidos se
    /// funden en el último: a la interfaz solo le importa dónde está ahora.
    fn queue(&mut self, event: UiEvent) {
        if matches!(event, UiEvent::Mouse(mouse::Event::CursorMoved { .. }))
            && matches!(
                self.pending.last(),
                Some(UiEvent::Mouse(mouse::Event::CursorMoved { .. }))
            )
        {
            self.pending.pop();
        }
        self.pending.push(event);
    }

    /// Construye la interfaz, le pasa `events` y, con `draw`, la dibuja en el motor de `iced`
    /// (lista para [`Compositor::draw_ui`]).
    fn pass(&mut self, events: &[UiEvent], draw: bool) -> Pass {
        let mut messages = Vec::new();
        let mut theme = self.app.theme;
        // Con el teclado y el ratón tomados no se anima nada (se leen en este hilo).
        theme.motion = if self.input_taken() {
            coroom_ui::motion::Motion::OFF
        } else {
            self.app.motion()
        };
        let mut interface = UserInterface::build(
            self.app.view(),
            self.viewport.logical_size(),
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let (state, statuses) = interface.update(
            events,
            self.cursor,
            &mut self.renderer,
            &mut self.clipboard,
            &mut messages,
        );
        let state = match state {
            user_interface::State::Updated {
                mouse_interaction,
                redraw_request,
                input_method,
                ..
            } => Some((mouse_interaction, redraw_request, input_method)),
            user_interface::State::Outdated => None,
        };
        let typing = chat::active(&self.app) && chat_focused(&mut interface, &self.renderer);
        if draw {
            interface.draw(
                &mut self.renderer,
                &theme,
                &renderer::Style {
                    text_color: theme.text_color(),
                },
                self.cursor,
            );
        }
        self.cache = interface.into_cache();
        Pass {
            messages,
            typing,
            state,
            statuses,
        }
    }

    /// Pasa ahora por la interfaz lo pendiente y `event` (un clic o la rueda con el vídeo
    /// delante: hay que saber si la interfaz lo usó antes de dárselo al anfitrión). Devuelve si
    /// la interfaz usó `event`.
    fn process_now(&mut self, event: UiEvent) -> bool {
        let mut events = std::mem::take(&mut self.pending);
        events.push(event);
        let pass = self.pass(&events, false);
        let captured = pass.statuses.last() == Some(&Status::Captured);
        for message in pass.messages {
            self.dispatch(message);
        }
        self.set_composing(pass.typing);
        self.window.request_redraw();
        captured
    }

    /// El campo del chat ganó o perdió el foco (un clic, `Tab`): la aplicación lo sabe y, al
    /// ganarlo, las teclas que el juego tuviera pulsadas se sueltan.
    fn set_composing(&mut self, typing: bool) {
        if typing == self.app.chat.composing {
            return;
        }
        if typing {
            self.release_game_keys();
        }
        self.dispatch(Message::Chat(chat::Msg::Composing(typing)));
    }

    /// Suelta en el anfitrión las teclas y botones pulsados (y el cursor capturado): el teclado
    /// pasa a la interfaz.
    fn release_game_keys(&mut self) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        let actions = input.capture.release_held();
        self.apply(actions);
    }

    /// Enfoca el campo del chat (o le quita el foco), como pide la aplicación.
    fn focus_chat(&mut self, on: bool) {
        use iced_core::widget::operation::{Operation, focusable};
        let mut interface = UserInterface::build(
            self.app.view(),
            self.viewport.logical_size(),
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let mut op: Box<dyn Operation> = if on {
            Box::new(focusable::focus(iced_core::widget::Id::new(chat::INPUT_ID)))
        } else {
            Box::new(focusable::unfocus())
        };
        interface.operate(&self.renderer, op.as_mut());
        self.cache = interface.into_cache();
        if on {
            self.release_game_keys();
        }
        self.window.request_redraw();
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
        self.sync_idle_inhibit();
    }

    /// Impide o vuelve a permitir el bloqueo del escritorio según [`App::wants_idle_inhibit`].
    fn sync_idle_inhibit(&mut self) {
        let wanted = self.app.wants_idle_inhibit();
        if wanted && self.idle_inhibit.is_none() {
            self.idle_inhibit = Some(crate::inhibit::IdleInhibitor::start(t!(
                "runner-transmision-en-curso"
            )));
        } else if !wanted {
            self.idle_inhibit = None;
        }
    }

    /// Guarda ya, en este hilo, los ajustes que cambiaron en los últimos cientos de ms y aún no se
    /// guardaron. Para los caminos de cierre: el proceso termina justo después y un guardado en
    /// segundo plano se perdería.
    fn save_settings_now(&mut self) {
        if self.app.flush_settings().is_some() {
            self.backend.save_settings(&self.app, true);
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
                Effect::FocusChat(on) => self.focus_chat(on),
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
        self.sync_in_front();
    }

    /// Tras cada cambio de la aplicación: si el vídeo dejó de estar delante (un diálogo como la
    /// oferta de traspaso, otra pantalla), el teclado y el ratón pasan a la interfaz y se sueltan
    /// en el anfitrión las teclas, el cursor y la tecla de pulsar para hablar.
    fn sync_in_front(&mut self) {
        let in_front = self.video_in_front();
        let Some(input) = self.input.as_mut() else {
            return;
        };
        let actions = input.capture.set_in_front(in_front);
        self.apply(actions);
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
            forwarding: false,
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
                Action::ToggleChat => {
                    self.dispatch(Message::Chat(super::screens::chat::Msg::Toggle));
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
        let forwarding = self.input.as_ref().map(|input| input.capture.forwarding());
        if let (Some(forwarding), Some(input)) = (forwarding, self.input.as_mut())
            && forwarding != input.forwarding
        {
            input.forwarding = forwarding;
            self.dispatch(Message::Guest(guest::Msg::ForwardingChanged(forwarding)));
            // El aviso del teclado y el ratón cambia de texto.
            self.window.request_redraw();
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

    /// Atajos `Ctrl+Alt` de la sala con el teclado en la interfaz (escribiendo en el chat): los
    /// mismos que con el vídeo delante, salvo el inmersivo.
    fn room_shortcut(&mut self, code: KeyCode) -> bool {
        // Los modificadores del sistema no cuentan AltGr como Ctrl ni Alt (en Linux es otro
        // modificador; en Windows `winit` filtra el Ctrl sintético y los quita): `AltGr+Q` escribe
        // `@` en el chat y no sale de la sala.
        if !(self.modifiers.control_key() && self.modifiers.alt_key()) {
            return false;
        }
        // Con un diálogo abierto (¿Salir de la sala?…) los atajos de la sala esperan.
        if self.app.dialog.is_some() {
            return false;
        }
        let keys = shortcut_keys(&self.app.settings.input.shortcuts);
        if code == keys.chat && chat::active(&self.app) {
            self.dispatch(Message::Chat(chat::Msg::Toggle));
            return true;
        }
        if self.app.guest.is_none() {
            return false;
        }
        if code == keys.overlay {
            self.dispatch(Message::Guest(guest::Msg::ToggleStats));
        } else if code == keys.pads {
            self.dispatch(Message::Guest(guest::Msg::TogglePads));
        } else if code == keys.quit {
            let effects = guest::leave(&mut self.app);
            self.run_effects(effects);
        } else if code == keys.release {
            if let Some(input) = self.input.as_mut() {
                let actions = input.capture.release_forwarding();
                self.apply(actions);
            }
        } else if let Some(input) = self.input.as_ref()
            && (code == keys.mic || code == keys.game)
        {
            if code == keys.mic {
                input.controls.audio.toggle_mic();
            } else {
                input.controls.audio.toggle_game();
            }
        } else {
            return false;
        }
        true
    }

    fn shortcut(&mut self, code: KeyCode) -> bool {
        if self.room_shortcut(code) {
            return true;
        }
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
                let typing =
                    chat::active(&self.app) && chat_focused(&mut interface, &self.renderer);
                self.cache = interface.into_cache();
                self.set_composing(typing);
                self.window.request_redraw();
                return true;
            }
            _ => return false,
        };
        self.dispatch(Message::Shortcut(shortcut));
        true
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        // Si la barra del vídeo aparece con este evento (un movimiento), hay que dibujarla.
        let bar_was_visible = self.bar_visible();
        match &event {
            WindowEvent::RedrawRequested => {
                self.redraw(event_loop);
                return;
            }
            WindowEvent::CloseRequested => {
                // Con la sala abierta (anfitrión o invitado) se pregunta, como con `Ctrl+Q`.
                if (self.app.host.is_open() && !self.app.probe) || self.app.in_room_as_guest() {
                    self.dispatch(Message::Shortcut(Shortcut::Quit));
                } else {
                    self.save_settings_now();
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
            WindowEvent::ScaleFactorChanged { .. } => {
                self.resized = true;
                self.reread_monitor();
            }
            WindowEvent::Moved(_) => self.reread_monitor(),
            WindowEvent::Focused(focused) => {
                self.dispatch(Message::WindowFocused(*focused));
                if *focused && system::reread_on_focus() {
                    self.reread_appearance(self.window.theme());
                }
            }
            WindowEvent::Occluded(hidden) => self.dispatch(Message::WindowOccluded(*hidden)),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                // La captura sigue Ctrl y Alt por sus teclas; al volver el foco (o si una
                // pulsación se perdió) se reajusta con lo que dice el sistema, sin enviar nada.
                if let Some(input) = self.input.as_mut() {
                    input
                        .capture
                        .set_modifiers(self.modifiers.control_key(), self.modifiers.alt_key());
                }
            }
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
            WindowEvent::ThemeChanged(theme) => self.reread_appearance(Some(*theme)),
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
        // Escribiendo en el chat, el teclado es de la interfaz aunque el vídeo esté delante.
        let typing = in_front && self.app.chat.composing;
        let grabbed = self
            .input
            .as_ref()
            .is_some_and(|input| input.capture.grabbed());
        // Sin reenviar (sin permiso, o suelto hasta el próximo clic), `Esc` es de la aplicación.
        let forwarding = self
            .input
            .as_ref()
            .is_some_and(|input| input.capture.forwarding());
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
                if in_front && !typing {
                    to_interface = false;
                    if code == Some(KeyCode::F11) {
                        if pressed && !key.repeat {
                            self.shortcut(KeyCode::F11);
                        }
                    } else if code == Some(KeyCode::Escape) && !forwarding {
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
        match &event {
            // Modo escritorio sobre el vídeo: la posición sale hacia el anfitrión ya, y la
            // interfaz la ve en el próximo dibujo, si es que le importa.
            WindowEvent::CursorMoved { .. } if capture_if_ignored => {
                if let Some(input) = self.input.as_mut() {
                    let actions = input.capture.on_window_event(&event);
                    self.apply(actions);
                }
                self.queue(converted);
                if !bar_was_visible || self.pointer_over_ui() {
                    self.window.request_redraw();
                }
            }
            // Un clic o la rueda sobre el vídeo (no sobre la barra) van al anfitrión.
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
                if capture_if_ignored =>
            {
                let captured = self.process_now(converted);
                if !captured && let Some(input) = self.input.as_mut() {
                    let actions = input.capture.on_window_event(&event);
                    self.apply(actions);
                }
            }
            _ => {
                self.queue(converted);
                self.window.request_redraw();
            }
        }
    }

    /// Si la barra del vídeo se ve ahora.
    fn bar_visible(&self) -> bool {
        self.app
            .guest
            .as_ref()
            .is_some_and(|guest| guest.bar_visible(self.app.now))
    }

    /// Con el vídeo delante, si a la interfaz le importa dónde está el cursor: sobre algo que
    /// reacciona (barra, panel, píldora, burbujas, toasts) o viniendo de algo que reaccionaba.
    fn pointer_over_ui(&self) -> bool {
        if self.interaction != mouse::Interaction::None {
            return true;
        }
        let Some(position) = self.cursor.position() else {
            return false;
        };
        guest::pointer_over_ui(&self.app, position, self.viewport.logical_size())
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

    /// Sincronización vertical como dice el ajuste.
    fn apply_vsync(&mut self) {
        let wanted = self.app.settings.video.vsync;
        if wanted != self.vsync_applied {
            self.vsync_applied = wanted;
            self.compositor.set_vsync(wanted);
            self.window.request_redraw();
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.last_frame = now;
        self.apply_hdr();
        self.apply_vsync();
        if self.resized {
            self.reconfigure();
        }
        self.app.now = now;
        let effects = self.app.update(Message::Tick);
        self.run_effects(effects);
        if self.should_exit() {
            event_loop.exit();
            return;
        }
        // Los eventos acumulados desde el último dibujo y el `RedrawRequested`, en una pasada.
        let mut events = std::mem::take(&mut self.pending);
        events.push(UiEvent::Window(window::Event::RedrawRequested(now)));
        let mut pass = self.pass(&events, true);
        self.apply_state(pass.state.take());
        if !pass.messages.is_empty() || pass.typing != self.app.chat.composing {
            // Los eventos cambiaron la aplicación: la interfaz se reconstruye con el estado nuevo
            // antes de dibujarla (como `iced_winit`), para no enseñar el de antes del clic.
            for message in pass.messages {
                self.dispatch(message);
            }
            self.set_composing(pass.typing);
            if self.should_exit() {
                event_loop.exit();
                return;
            }
            pass = self.pass(
                &[UiEvent::Window(window::Event::RedrawRequested(now))],
                true,
            );
            self.apply_state(pass.state.take());
        }
        // La interfaz va a su textura (transparente donde no dibuja nada: el vídeo se ve
        // debajo) y el compositor la pone sobre el fondo y el vídeo y entrega; con el vídeo
        // delante y llegando, la entrega el próximo fotograma ([`Present::WithVideo`]).
        let video = if self.app.video_in_front() {
            self.compositor.video_interval(now)
        } else {
            None
        };
        let present = if video.is_some() {
            Present::WithVideo
        } else {
            Present::Now
        };
        let format = self.render_format;
        let renderer = &mut self.renderer;
        let viewport = &self.viewport;
        let window = &self.window;
        self.compositor.draw_ui(
            self.app.backdrop(),
            present,
            |target| {
                let _ = renderer.present(Some(Color::TRANSPARENT), format, target, viewport);
            },
            // En Wayland, el siguiente `RedrawRequested` espera la señal de cuadro del compositor.
            || window.pre_present_notify(),
        );
        self.app.ui_draws.record(now.elapsed());
        if !pass.messages.is_empty() {
            for message in pass.messages {
                self.dispatch(message);
            }
            self.window.request_redraw();
        }
        self.set_composing(pass.typing);
        self.schedule_frame(now, video);
    }

    /// Pide el siguiente cuadro de animación al ritmo del compositor ([`pacing`]), si algo se
    /// mueve. Con el teclado y el ratón tomados o en modo inmersivo no se anima nada: se leen en
    /// este mismo hilo; con la ventana oculta, tampoco. Un dibujo que quedó esperando al vídeo
    /// (`video`) sale solo si el vídeo se detiene.
    fn schedule_frame(&mut self, now: Instant, video: Option<Duration>) {
        let animating = (self.app.animating() || std::mem::take(&mut self.ui_frame))
            && !self.input_taken()
            && !self.app.window_hidden;
        if !animating && video.is_none() {
            return;
        }
        let next = pacing::next_frame(
            self.clock,
            now,
            self.last_frame,
            video,
            pacing::desktop_vblank,
        );
        match next {
            NextFrame::Request => self.window.request_redraw(),
            NextFrame::At(at) => self.redraw_at(at),
            NextFrame::WithVideo { fallback } => {
                if animating {
                    self.compositor.want_ui_frame();
                }
                self.video_fallback = Some(fallback);
            }
            NextFrame::Never => {}
        }
    }

    fn redraw_at(&mut self, at: Instant) {
        self.redraw_at = Some(self.redraw_at.map_or(at, |old| old.min(at)));
    }

    /// Si el invitado tiene el teclado y el ratón tomados o está en modo inmersivo.
    fn input_taken(&self) -> bool {
        self.input
            .as_ref()
            .is_some_and(|input| input.capture.grabbed())
            || self.app.guest.as_ref().is_some_and(|guest| guest.immersive)
    }

    /// Vuelve a leer la apariencia del sistema; `theme` es el modo que da la ventana si el sistema
    /// no lo informa.
    fn reread_appearance(&mut self, theme: Option<winit::window::Theme>) {
        let mut appearance = system::read();
        if appearance.mode.is_none() {
            appearance.mode = theme.map(|theme| match theme {
                winit::window::Theme::Dark => coroom_ui::theme::Mode::Dark,
                winit::window::Theme::Light => coroom_ui::theme::Mode::Light,
            });
        }
        self.dispatch(Message::System(appearance));
    }

    /// El monitor de la ventana pudo cambiar: con [`FrameClock::Monitor`], su período de nuevo.
    fn reread_monitor(&mut self) {
        if let FrameClock::Monitor(_) = self.clock {
            self.clock = FrameClock::Monitor(monitor_period(&self.window));
        }
    }

    /// Lo que la interfaz quiere de la ventana tras una pasada: el icono del cursor, cuándo volver
    /// a dibujar y el método de entrada.
    fn apply_state(
        &mut self,
        state: Option<(mouse::Interaction, window::RedrawRequest, InputMethod)>,
    ) {
        let Some((mouse_interaction, redraw_request, input_method)) = state else {
            return;
        };
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
            // Un componente se está animando: el siguiente cuadro, al ritmo del compositor.
            window::RedrawRequest::NextFrame => self.ui_frame = true,
            window::RedrawRequest::At(at) => self.redraw_at(at),
            window::RedrawRequest::Wait => {}
        }
        self.input_method(input_method);
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
        consider(self.video_fallback);
        if self.input.is_some() {
            consider(Some(self.next_sync));
        }
        next
    }
}

impl ApplicationHandler<UserEvent> for Runner {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        let Some(ready) = self.ready.as_mut() else {
            return;
        };
        if let StartCause::ResumeTimeReached { .. } = cause {
            let now = Instant::now();
            // Si lo único que tocaba era el respaldo del vídeo y el vídeo ya entregó la interfaz,
            // no hay nada que dibujar.
            let mut redraw = ready.app.next_tick().is_some_and(|at| at <= now);
            if ready.redraw_at.is_some_and(|at| at <= now) {
                ready.redraw_at = None;
                redraw = true;
            }
            if ready.video_fallback.is_some_and(|at| at <= now) {
                ready.video_fallback = None;
                redraw |= ready.compositor.ui_pending();
            }
            if ready.input.is_some() && now >= ready.next_sync {
                ready.next_sync = now + INPUT_SYNC;
                ready.sync_input();
                redraw = true;
            }
            // Los plazos de la aplicación corren aunque la ventana no dibuje (oculta, el
            // compositor no manda su señal de cuadro): el `Tick` va aquí, no solo al dibujar.
            ready.app.now = now;
            let effects = ready.app.update(Message::Tick);
            ready.run_effects(effects);
            if ready.should_exit() {
                event_loop.exit();
                return;
            }
            if redraw {
                ready.window.request_redraw();
            }
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
            ready.save_settings_now();
            ready.detach_input();
            self.result = ready.backend.shutdown();
        }
    }
}

/// Quién marca el ritmo de las animaciones en esta ventana ([`pacing`]).
fn frame_clock(window: &Window) -> FrameClock {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let wayland = window
        .window_handle()
        .is_ok_and(|handle| matches!(handle.as_raw(), RawWindowHandle::Wayland(_)));
    if wayland {
        FrameClock::Compositor
    } else if cfg!(target_os = "windows") {
        FrameClock::Desktop
    } else {
        FrameClock::Monitor(monitor_period(window))
    }
}

/// Período del monitor en el que está la ventana, si lo informa.
fn monitor_period(window: &Window) -> Option<Duration> {
    FrameClock::period(
        window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz()),
    )
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
        assert_eq!(keys.overlay, KeyCode::KeyS);
        assert_eq!(keys.chat, KeyCode::KeyC);
        assert_eq!(keys.release, KeyCode::KeyR);
    }
}
