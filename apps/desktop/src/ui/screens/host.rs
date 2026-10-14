//! Pantalla 4: anfitrión.
//!
//! Antes de abrir, qué compartir (el boceto aprobado): dos tarjetas, **Transmitir pantalla** (un
//! monitor, con el audio de todas las aplicaciones salvo las que quites, que se recuerdan) y
//! **Transmitir una app** (una ventana, con el audio solo de esa aplicación), más la fuente de
//! prueba como enlace; el monitor o la ventana elegidos con "Cambiar…"; la lista de audio, la voz y
//! la calidad. La lista de audio es la misma en todos los sistemas: las aplicaciones que suenan,
//! refrescadas cada [`REFRESH`]. Donde el sistema elige el monitor o la ventana con su propio
//! selector (el portal en Wayland), "Elegir…" lo abre ya y la elección se repite al empezar con su
//! token; donde no, se elige aquí de la lista. Después, la sala abierta: el código grande con copiar, la vista previa de lo
//! que se comparte, los invitados con sus permisos como interruptores (teclado y ratón, mando con
//! ranura, voz), sus estadísticas (ruta, RTT, bitrate, pérdida, latencia de entrada), pausar a
//! todos, expulsar, invitar a amigos conectados y "Dejar de compartir".

use std::sync::Arc;
use std::time::Duration;

use coroom_session::audio::GameCapture;
use coroom_session::audio::apps::{AppFilter, PlayingApp, normalize};
use coroom_session::capture::{CaptureTarget, PickedShare, ShareChoice, ShareKind};
use coroom_session::remote::{GuestStatus, PadTransport, SessionEvent, permissions_label};
use coroom_session::{GuestId, Permissions};
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, button, checkbox, column, container, row};

use crate::ui::app::{App, Dialog, Effect, Event, HostRequest, Load, Message, Page};
use crate::ui::screens::settings as settings_screen;
use crate::ui::settings::{CodecChoice, KnownPeer, RememberedApp};
use crate::ui::stats;
use crate::ui::video::{VideoFeed, video_view};

/// Cada cuánto se vuelve a mirar qué suena y qué se puede compartir.
pub const REFRESH: Duration = Duration::from_secs(2);

/// Aplicaciones que casi nunca son el juego: no se marcan solas al transmitir una app cuando el
/// sistema no dice de quién es la ventana.
const NOT_GAMES: [&str; 16] = [
    "firefox", "chrome", "chromium", "brave", "msedge", "discord", "vesktop", "steam", "spotify",
    "telegram", "slack", "teams", "zoom", "obs", "signal", "webcord",
];

/// Fuente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceChoice {
    /// Un monitor entero, con el audio de todas las aplicaciones elegidas.
    #[default]
    Screen,
    /// Una ventana, con el audio solo de su aplicación.
    App,
    /// Patrón sintético.
    Synthetic,
}

impl SourceChoice {
    /// Qué se comparte (la fuente de prueba, como la pantalla).
    pub fn kind(self) -> ShareKind {
        match self {
            SourceChoice::App => ShareKind::App,
            SourceChoice::Screen | SourceChoice::Synthetic => ShareKind::Screen,
        }
    }
}

/// Monitor o ventana en el selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetOption(pub CaptureTarget);

impl std::fmt::Display for TargetOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            CaptureTarget::Monitor { name, .. } => f.write_str(name),
            CaptureTarget::Window { title, .. } => f.write_str(title),
        }
    }
}

/// Lo que se puede compartir ahora.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareOptions {
    /// Para qué fuente se pidió.
    pub source: SourceChoice,
    /// Monitores o ventanas (vacío: los elige el sistema al empezar).
    pub targets: Vec<CaptureTarget>,
    /// Por qué no se pudieron listar los monitores o las ventanas (`targets` va vacío).
    pub targets_error: Option<String>,
    /// Aplicaciones que suenan (o por qué no se puede elegir el audio por aplicación).
    pub apps: Result<Vec<PlayingApp>, String>,
}

/// Una fila de la lista de audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRow {
    /// Clave de la aplicación.
    pub key: String,
    /// Nombre.
    pub name: String,
    /// Si se envía.
    pub checked: bool,
    /// Nota a la derecha.
    pub note: &'static str,
}

/// Ranura de mando en el selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadSlot(pub Option<u8>);

impl std::fmt::Display for PadSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            None => f.write_str(t!("pantalla-host-sin-mando")),
            Some(slot) => write!(f, "{}", t!("pantalla-host-ranura-slot", slot = slot)),
        }
    }
}

/// Cómo llega el mando de un invitado: tal cual (`true`) o convertido a Xbox 360.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadMode(pub bool);

impl std::fmt::Display for PadMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0 {
            t!("pantalla-host-tal-cual")
        } else {
            t!("pantalla-host-convertido-a-xbox-360")
        })
    }
}

/// Sala abierta.
#[derive(Debug, Clone)]
pub struct Room {
    /// Código (cuando el servidor la crea).
    pub code: Option<String>,
    /// Huella del anfitrión.
    pub fingerprint: String,
    /// Códec en uso.
    pub codec: Option<String>,
    /// Invitados.
    pub guests: Vec<GuestStatus>,
    /// Entrada en pausa.
    pub paused: bool,
    /// Vista previa.
    pub preview: Arc<VideoFeed>,
    /// Sin vista previa (la captura va por la GPU).
    pub preview_unavailable: bool,
    /// Sin relay (cuota o servidor).
    pub relay: Option<String>,
}

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Fuente.
    pub source: SourceChoice,
    /// Monitor o ventana elegido (si el sistema deja elegirlo aquí).
    pub target: Option<CaptureTarget>,
    /// Lo elegido en el selector del sistema (Wayland), para la fuente actual.
    pub picked: Option<PickedShare>,
    /// Esperando al selector del sistema.
    pub picking: bool,
    /// Lo que se puede compartir (`None`: aún no se ha mirado).
    pub options: Option<ShareOptions>,
    /// Mirando qué se puede compartir.
    pub refreshing: bool,
    /// Cuándo se miró por última vez.
    pub refreshed_at: Option<std::time::Instant>,
    /// Al transmitir una app: las aplicaciones cuyo audio se envía.
    pub only: Vec<String>,
    /// Si [`State::only`] lo eligió la aplicación (se rehace al cambiar de ventana o de lo que
    /// suena) o el usuario.
    pub only_chosen: bool,
    /// Voz.
    pub voice: bool,
    /// Abriendo.
    pub starting: bool,
    /// Cerrando.
    pub stopping: bool,
    /// Error al abrir.
    pub error: Option<String>,
    /// Sala abierta.
    pub room: Option<Room>,
    /// Lo que se transmite desde que se abrió la sala (o desde el último cambio).
    pub shared: Option<Sharing>,
    /// Cambiando lo que se comparte con la sala abierta: lo pedido.
    pub switching: Option<Sharing>,
}

/// Fuente y monitor o ventana de una transmisión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharing {
    /// Fuente.
    pub source: SourceChoice,
    /// Monitor o ventana de la lista.
    pub target: Option<CaptureTarget>,
    /// Lo elegido en el selector del sistema.
    pub picked: Option<PickedShare>,
}

impl Sharing {
    /// Si es lo mismo (el título de una ventana puede cambiar sin que cambie la ventana).
    fn same(&self, other: &Sharing) -> bool {
        let targets = match (&self.target, &other.target) {
            (Some(a), Some(b)) => same_target(a, b),
            (None, None) => true,
            _ => false,
        };
        self.source == other.source && targets && self.picked == other.picked
    }
}

impl State {
    /// Si hay una sala abierta (o abriéndose).
    pub fn is_open(&self) -> bool {
        self.room.is_some() || self.starting
    }

    /// Vista previa, si hay sala.
    pub fn preview(&self) -> Option<Arc<VideoFeed>> {
        self.room.as_ref().map(|room| Arc::clone(&room.preview))
    }

    /// Cuándo toca volver a mirar qué se puede compartir (antes de abrir la sala y con ella
    /// abierta, para cambiar de fuente): `now` si nunca se ha mirado, `None` mientras se abre o se
    /// mira. Es el plazo que espera la ventana; [`State::refresh_due`] dice si ya llegó.
    pub fn next_refresh(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        if self.starting || self.refreshing {
            return None;
        }
        Some(self.refreshed_at.map_or(now, |at| at + REFRESH))
    }

    /// Si toca volver a mirar qué se puede compartir.
    pub fn refresh_due(&self, now: std::time::Instant) -> bool {
        !self.starting && !self.refreshing && self.refreshed_at.is_none_or(|at| now >= at + REFRESH)
    }

    /// Lo elegido ahora en la pantalla.
    pub fn sharing(&self) -> Sharing {
        Sharing {
            source: self.source,
            target: self.target.clone(),
            picked: self.picked.clone(),
        }
    }

    /// Con la sala abierta, si lo elegido no es lo que se transmite.
    pub fn source_changed(&self) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|shared| !shared.same(&self.sharing()))
    }

    /// Lo que se pide capturar.
    fn share_choice(&self) -> ShareChoice {
        ShareChoice {
            kind: self.source.kind(),
            target: self
                .picked
                .as_ref()
                .map(|picked| picked.target.clone())
                .or_else(|| self.target.clone()),
            restore_token: self
                .picked
                .as_ref()
                .and_then(|picked| picked.restore_token.clone()),
        }
    }

    /// Lo que se puede compartir con la fuente elegida (`None`: aún no se ha mirado para ella).
    fn current_options(&self) -> Option<&ShareOptions> {
        self.options
            .as_ref()
            .filter(|options| options.source == self.source)
    }

    /// Aplicaciones que suenan, si el audio por aplicación funciona.
    fn playing(&self) -> Option<&[PlayingApp]> {
        self.current_options()
            .and_then(|options| options.apps.as_ref().ok())
            .map(Vec::as_slice)
    }

    /// Si el monitor o la ventana los elige el selector del sistema (no hay lista aquí).
    fn system_picks(&self) -> bool {
        self.current_options()
            .is_some_and(|options| options.targets.is_empty() && options.targets_error.is_none())
    }

    /// Clave de la aplicación de la ventana elegida, si el sistema la dice.
    fn target_app(&self) -> Option<&str> {
        match &self.target {
            Some(CaptureTarget::Window { app: Some(app), .. }) => Some(app),
            _ => None,
        }
    }

    /// Rehace la selección automática de la app: la de la ventana o, si el sistema no la dice,
    /// la primera que suena y no parece un navegador, un chat o una tienda.
    fn auto_select(&mut self) {
        // Con la sala abierta, el audio de lo que ya se transmite no cambia solo.
        let live = self.room.is_some() && !self.source_changed();
        if self.only_chosen || self.source != SourceChoice::App || live {
            return;
        }
        let guess = self.target_app().map(normalize).or_else(|| {
            self.playing()?
                .iter()
                .find(|app| !NOT_GAMES.iter().any(|word| app.key.contains(word)))
                .map(|app| app.key.clone())
        });
        self.only = guess.into_iter().collect();
    }

    /// Filas de la lista de audio, con las aplicaciones quitadas que se recuerdan.
    pub fn audio_rows(&self, excluded: &[RememberedApp]) -> Vec<AudioRow> {
        let playing = self.playing().unwrap_or_default();
        let is_excluded = |key: &str| excluded.iter().any(|app| normalize(&app.key) == key);
        let mut rows: Vec<AudioRow> = Vec::new();
        match self.source {
            SourceChoice::App => {
                let chosen = self.target_app().map(normalize);
                if let Some(key) = &chosen
                    && !playing.iter().any(|app| &app.key == key)
                {
                    rows.push(AudioRow {
                        key: key.clone(),
                        name: key.trim_end_matches(".exe").to_owned(),
                        checked: self.only.contains(key),
                        note: t!("pantalla-host-la-aplicacion-elegida-aun-sin-sonido"),
                    });
                }
                rows.extend(playing.iter().map(|app| AudioRow {
                    key: app.key.clone(),
                    name: app.name.clone(),
                    checked: self.only.contains(&app.key),
                    note: if chosen.as_ref() == Some(&app.key) {
                        t!("pantalla-host-la-aplicacion-elegida")
                    } else {
                        t!("pantalla-host-sonando")
                    },
                }));
            }
            SourceChoice::Screen | SourceChoice::Synthetic => {
                rows.extend(playing.iter().map(|app| {
                    let out = is_excluded(&app.key);
                    AudioRow {
                        key: app.key.clone(),
                        name: app.name.clone(),
                        checked: !out,
                        note: if out {
                            t!("pantalla-host-quitada-siempre")
                        } else {
                            t!("pantalla-host-sonando")
                        },
                    }
                }));
                rows.extend(
                    excluded
                        .iter()
                        .filter(|app| !playing.iter().any(|p| p.key == normalize(&app.key)))
                        .map(|app| AudioRow {
                            key: normalize(&app.key),
                            name: app.name.clone(),
                            checked: false,
                            note: t!("pantalla-host-quitada-siempre"),
                        }),
                );
            }
        }
        rows
    }

    /// Audio del juego que se pide al abrir la sala.
    pub fn game_capture(&self, excluded: &[RememberedApp]) -> GameCapture {
        if self.playing().is_none() {
            // Sin audio por aplicación en este equipo: todo lo que suena.
            return GameCapture::System;
        }
        match self.source {
            // Aunque no se marque ninguna, la captura por aplicación arranca: así se puede marcar
            // una con la sala abierta.
            SourceChoice::App => GameCapture::Apps(AppFilter::Only(self.only.clone())),
            SourceChoice::Screen | SourceChoice::Synthetic => GameCapture::Apps(
                AppFilter::AllExcept(excluded.iter().map(|app| normalize(&app.key)).collect()),
            ),
        }
    }
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Fuente.
    Source(SourceChoice),
    /// Monitor o ventana.
    Target(TargetOption),
    /// Enviar o no el audio de una aplicación (clave, nombre).
    AppAudio(String, String, bool),
    /// Voz.
    Voice(bool),
    /// Abrir el selector del sistema para elegir el monitor o la ventana.
    Pick,
    /// Volver a Inicio sin abrir la sala.
    Cancel,
    /// Abrir la sala.
    Start,
    /// Copiar el código.
    CopyCode,
    /// Dejar de compartir (pide confirmación).
    Stop,
    /// Pausar o reanudar la entrada de todos.
    Pause(bool),
    /// Teclado y ratón de un invitado.
    Keyboard(GuestId, bool),
    /// Mando de un invitado.
    Pad(GuestId, PadSlot),
    /// Si el mando de un invitado va tal cual o convertido.
    PadMode(GuestId, PadMode),
    /// Voz de un invitado.
    GuestVoice(GuestId, bool),
    /// Expulsar (pide confirmación).
    Kick(GuestId),
    /// Invitar a un amigo conectado.
    Invite(u64),
    /// Número de seguridad de un invitado.
    Safety(GuestId),
    /// Ir a Ajustes → Vídeo.
    VideoSettings,
    /// Con la sala abierta: compartir lo elegido en vez de lo de ahora.
    ApplySource,
}

fn guest(app: &App, id: GuestId) -> Option<&GuestStatus> {
    app.host
        .room
        .as_ref()
        .and_then(|room| room.guests.iter().find(|status| status.guest == id))
}

fn set_permissions(
    app: &mut App,
    id: GuestId,
    change: impl FnOnce(&mut Permissions),
) -> Vec<Effect> {
    let Some(room) = app.host.room.as_mut() else {
        return Vec::new();
    };
    let Some(status) = room.guests.iter_mut().find(|status| status.guest == id) else {
        return Vec::new();
    };
    change(&mut status.permissions);
    vec![Effect::SetPermissions(id, status.permissions)]
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Source(source) => {
            if app.host.source != source {
                app.host.source = source;
                app.host.target = None;
                app.host.picked = None;
                app.host.only_chosen = false;
                app.host.only.clear();
                app.host.auto_select();
                return refresh(app);
            }
        }
        Msg::Target(TargetOption(target)) => {
            app.host.target = Some(target);
            app.host.only_chosen = false;
            app.host.auto_select();
        }
        Msg::AppAudio(key, name, on) => {
            let key = normalize(&key);
            let mut effects = Vec::new();
            if app.host.source == SourceChoice::App {
                app.host.only.retain(|known| *known != key);
                if on {
                    app.host.only.push(key);
                }
                app.host.only_chosen = true;
            } else {
                // Al transmitir la pantalla, quitar una aplicación se recuerda.
                let excluded = &mut app.settings.audio.excluded_apps;
                excluded.retain(|known| normalize(&known.key) != key);
                if !on {
                    excluded.push(RememberedApp { key, name });
                }
                effects.push(Effect::SaveSettings);
            }
            // Con la sala abierta, el audio de lo que se transmite cambia ya (si se está eligiendo
            // otra fuente, va con ella al cambiar).
            if app.host.room.is_some() && !app.host.source_changed() {
                effects.push(Effect::SetAppAudio(
                    app.host.game_capture(&app.settings.audio.excluded_apps),
                ));
            }
            return effects;
        }
        Msg::Voice(voice) => app.host.voice = voice,
        Msg::Pick => {
            if app.host.picking || app.host.starting || app.host.switching.is_some() {
                return Vec::new();
            }
            app.host.picking = true;
            return vec![Effect::PickShare(app.host.source)];
        }
        Msg::Cancel => return app.update(Message::Navigate(Page::Home)),
        Msg::Start => {
            if app.host.is_open() {
                return Vec::new();
            }
            let audio = app.host.game_capture(&app.settings.audio.excluded_apps);
            app.host.starting = true;
            app.host.error = None;
            app.host.shared = Some(app.host.sharing());
            app.host.switching = None;
            // La sala de espera sigue hasta que la sala esté abierta: así los amigos te siguen
            // viendo conectado mientras arranca (y si no arranca).
            return vec![Effect::StartHost(HostRequest {
                source: app.host.source,
                share: app.host.share_choice(),
                audio,
                voice: app.host.voice,
            })];
        }
        Msg::CopyCode => {
            if let Some(code) = app.host.room.as_ref().and_then(|room| room.code.clone()) {
                return app.update(Message::Copy(code));
            }
        }
        Msg::Stop => app.dialog = Some(Dialog::StopSharing),
        Msg::Pause(paused) => {
            if let Some(room) = app.host.room.as_mut() {
                room.paused = paused;
                return vec![Effect::PauseInput(paused)];
            }
        }
        Msg::Keyboard(id, on) => return set_permissions(app, id, |p| p.keyboard_mouse = on),
        Msg::Pad(id, slot) => {
            let taken = slot.0.is_some_and(|wanted| {
                app.host.room.as_ref().is_some_and(|room| {
                    room.guests
                        .iter()
                        .any(|g| g.guest != id && g.permissions.gamepad_slot == Some(wanted))
                })
            });
            if taken {
                app.toast(
                    Tone::Warning,
                    t!("pantalla-host-la-slot-ya-es-de-otro", slot = slot),
                );
                return Vec::new();
            }
            return set_permissions(app, id, |p| p.gamepad_slot = slot.0);
        }
        Msg::PadMode(id, mode) => return set_permissions(app, id, |p| p.gamepad_raw = mode.0),
        Msg::GuestVoice(id, on) => return set_permissions(app, id, |p| p.voice = on),
        Msg::Kick(id) => {
            if let Some(status) = guest(app, id) {
                app.dialog = Some(Dialog::Kick {
                    guest: id,
                    name: status.name.clone(),
                });
            }
        }
        Msg::Invite(user) => return app.update(Message::Home(super::home::Msg::Invite(user))),
        Msg::Safety(id) => {
            if let Some(status) = guest(app, id) {
                app.dialog = Some(Dialog::SafetyNumber {
                    name: status.name.clone(),
                    fingerprint: status.fingerprint.clone(),
                    number: status.safety_number.to_string(),
                });
            }
        }
        Msg::VideoSettings => {
            app.settings_page.page = super::settings::SettingsPage::Video;
            return app.update(Message::Navigate(Page::Settings));
        }
        Msg::ApplySource => return apply_source(app),
    }
    Vec::new()
}

/// Con la sala abierta, pide compartir lo elegido (con el audio que le toca).
fn apply_source(app: &mut App) -> Vec<Effect> {
    let host = &mut app.host;
    if host.room.is_none() || host.switching.is_some() || !host.source_changed() {
        return Vec::new();
    }
    host.switching = Some(host.sharing());
    vec![Effect::SwitchSource(HostRequest {
        source: host.source,
        share: host.share_choice(),
        audio: host.game_capture(&app.settings.audio.excluded_apps),
        voice: host.voice,
    })]
}

/// Pide mirar otra vez qué se puede compartir.
pub fn refresh(app: &mut App) -> Vec<Effect> {
    if app.host.starting {
        return Vec::new();
    }
    app.host.refreshing = true;
    vec![Effect::LoadShareOptions(app.host.source)]
}

/// Lo que se puede compartir, recién mirado.
pub fn on_share_options(app: &mut App, options: ShareOptions) {
    app.host.refreshing = false;
    app.host.refreshed_at = Some(app.now);
    if options.source != app.host.source {
        // Llegó tarde, para la fuente de antes: se vuelve a mirar en el próximo `Tick`.
        app.host.refreshed_at = None;
        return;
    }
    // Si no se pudo listar qué compartir, se avisa una vez (la lista se mira cada pocos
    // segundos) y la sección lo explica mientras dure.
    let known = app
        .host
        .current_options()
        .and_then(|old| old.targets_error.as_ref());
    if let Some(err) = &options.targets_error
        && known != Some(err)
    {
        let text = if options.source == SourceChoice::App {
            t!(
                "pantalla-host-no-se-pudieron-listar-las-ventanas",
                error = err
            )
        } else {
            t!(
                "pantalla-host-no-se-pudieron-listar-los-monitores",
                error = err
            )
        };
        app.toast(Tone::Warning, text);
    }
    let host = &mut app.host;
    // El destino elegido se mantiene si sigue existiendo (con su título de ahora); si no, el
    // primero.
    host.target = match &host.target {
        Some(chosen) => options
            .targets
            .iter()
            .find(|target| same_target(target, chosen))
            .or(options.targets.first())
            .cloned(),
        None => options.targets.first().cloned(),
    };
    host.options = Some(options);
    host.auto_select();
}

/// Lo elegido en el selector del sistema.
///
/// Con la sala abierta, lo elegido se comparte enseguida: el diálogo del sistema ya fue la
/// confirmación.
pub fn on_share_picked(
    app: &mut App,
    source: SourceChoice,
    picked: Result<Option<PickedShare>, String>,
) -> Vec<Effect> {
    app.host.picking = false;
    if source != app.host.source {
        return Vec::new();
    }
    match picked {
        Ok(Some(picked)) => {
            app.host.picked = Some(picked);
            app.host.only_chosen = false;
            app.host.auto_select();
            if app.host.room.is_some() {
                return apply_source(app);
            }
        }
        Ok(None) => {}
        Err(err) => app.toast(
            Tone::Warning,
            t!("pantalla-host-no-se-pudo-abrir-el-selector", error = err),
        ),
    }
    Vec::new()
}

/// Si dos destinos son el mismo monitor o ventana.
fn same_target(a: &CaptureTarget, b: &CaptureTarget) -> bool {
    match (a, b) {
        (CaptureTarget::Monitor { id: a, .. }, CaptureTarget::Monitor { id: b, .. })
        | (CaptureTarget::Window { id: a, .. }, CaptureTarget::Window { id: b, .. }) => a == b,
        _ => false,
    }
}

/// Ranuras que se ofrecen, la preferida primero.
pub fn pad_choices(preferred: u8, max: u8) -> Vec<PadSlot> {
    let mut slots: Vec<PadSlot> = (1..=max).map(|slot| PadSlot(Some(slot))).collect();
    if (1..=max).contains(&preferred) {
        slots.sort_by_key(|slot| slot.0 != Some(preferred));
    }
    slots.insert(0, PadSlot(None));
    slots
}

/// Resultados de la sesión del anfitrión.
pub fn on_event(app: &mut App, event: Event) -> Vec<Effect> {
    match event {
        Event::HostStarted(Ok(preview)) => {
            app.host.starting = false;
            app.host.room = Some(Room {
                code: None,
                fingerprint: String::new(),
                codec: None,
                guests: Vec::new(),
                paused: false,
                preview,
                preview_unavailable: false,
                relay: None,
            });
            app.page = Page::Host;
            // La sala lleva su propia señalización: la de espera ya sobra.
            return vec![Effect::StopLobby];
        }
        Event::HostStarted(Err(err)) => {
            app.host.starting = false;
            app.host.shared = None;
            app.host.error = Some(err);
            return vec![Effect::StartLobby];
        }
        Event::HostGuests(guests) => {
            if let Some(room) = app.host.room.as_mut() {
                room.guests = guests;
            }
        }
        Event::PreviewUnavailable => {
            if let Some(room) = app.host.room.as_mut() {
                room.preview_unavailable = true;
            }
        }
        Event::HostEnded(reason) => {
            let was_open = app.host.room.take().is_some();
            app.host.shared = None;
            app.host.switching = None;
            app.host.starting = false;
            app.host.stopping = false;
            if let Some(reason) = reason {
                app.toast(
                    Tone::Error,
                    t!("pantalla-host-la-sala-se-cerro-reason", reason = reason),
                );
            } else if was_open {
                app.toast(Tone::Info, t!("pantalla-host-has-dejado-de-compartir"));
            }
            if app.page == Page::Host && !was_open {
                app.page = Page::Home;
            }
            return if app.account.is_some() {
                vec![Effect::StartLobby]
            } else {
                Vec::new()
            };
        }
        Event::Host(event) => return session_event(app, event),
        Event::SourceSwitched(result) => {
            let requested = app.host.switching.take();
            if app.host.room.is_none() {
                return Vec::new();
            }
            match result {
                Ok(()) => {
                    app.toast(Tone::Info, t!("pantalla-host-ahora-compartes-otra-cosa"));
                    if requested.is_some() {
                        app.host.shared = requested;
                    }
                }
                Err(err) => app.toast(
                    Tone::Warning,
                    t!("pantalla-host-no-se-pudo-cambiar-la-fuente", error = err),
                ),
            }
        }
        _ => {}
    }
    Vec::new()
}

fn session_event(app: &mut App, event: SessionEvent) -> Vec<Effect> {
    let mut effects = Vec::new();
    let Some(room) = app.host.room.as_mut() else {
        return effects;
    };
    match event {
        SessionEvent::RoomCreated { code, fingerprint } => {
            room.code = Some(code);
            room.fingerprint = fingerprint;
        }
        SessionEvent::GuestJoined {
            name,
            fingerprint,
            safety_number,
            permissions,
            ..
        } => {
            app.settings.remember_peer(KnownPeer {
                name: name.clone(),
                fingerprint,
                safety_number: safety_number.to_string(),
            });
            effects.push(Effect::SaveSettings);
            app.toast(
                Tone::Info,
                t!(
                    "pantalla-host-name-ha-entrado-en-la-sala",
                    name = name,
                    valor = permissions_label(&permissions)
                ),
            );
        }
        SessionEvent::GuestLeft { guest, kicked } => {
            let name = room
                .guests
                .iter()
                .find(|status| status.guest == guest)
                .map_or_else(
                    || t!("pantalla-host-el-invitado", valor = guest.0),
                    |s| s.name.clone(),
                );
            room.guests.retain(|status| status.guest != guest);
            app.toast(
                Tone::Neutral,
                if kicked {
                    t!(
                        "pantalla-host-name-ha-sido-expulsado-las-claves",
                        name = name
                    )
                } else {
                    t!("pantalla-host-name-ha-salido-de-la-sala", name = name)
                },
            );
        }
        SessionEvent::VideoCodec { codec, detail } => {
            room.codec = Some(format!(
                "{codec} ({})",
                detail.trim_start_matches(t!("pantalla-host-codifica"))
            ));
        }
        SessionEvent::RelayUnavailable(message) => room.relay = Some(message),
        SessionEvent::Warning(message) => app.toast(Tone::Warning, message),
        _ => {}
    }
    effects
}

fn on(msg: Msg) -> Message {
    Message::Host(msg)
}

/// Dibuja la página.
pub fn view(app: &App) -> Element<'_, Message> {
    match &app.host.room {
        Some(room) => room_view(app, room),
        None => setup(app),
    }
}

/// Tarjeta de una de las dos fuentes, con su icono y el borde de acento si es la elegida. Sin
/// mensaje es la copia que solo da la altura en [`source_cards`].
fn choice<'a>(
    icon: Icon,
    title: &'a str,
    subtitle: &'a str,
    selected: bool,
    message: Option<Message>,
) -> Element<'a, Message> {
    let class = if selected {
        SvgClass::Accent
    } else {
        SvgClass::Secondary
    };
    let content = row![
        icon_with(icon, 28.0, class),
        column![
            ui::strong(title),
            ui::secondary(subtitle).wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(space::XXS)
        .width(Length::Fill),
    ]
    .spacing(space::S)
    .align_y(Alignment::Start);
    button(content)
        .padding(space::M)
        .width(Length::Fill)
        .height(if message.is_some() {
            Length::Fill
        } else {
            Length::Shrink
        })
        .class(ButtonClass::Choice { selected })
        .on_press_maybe(message)
        .into()
}

/// Las dos tarjetas de fuente, igual de altas.
fn source_cards(state: &State) -> Element<'_, Message> {
    ui::equal_height_row(
        |real| {
            let message = |source| real.then(|| on(Msg::Source(source)));
            vec![
                choice(
                    Icon::Monitor,
                    t!("pantalla-host-transmitir-pantalla"),
                    t!("pantalla-host-un-monitor-entero-con-el-audio"),
                    state.source == SourceChoice::Screen,
                    message(SourceChoice::Screen),
                ),
                choice(
                    Icon::AppWindow,
                    t!("pantalla-host-transmitir-una-app"),
                    t!("pantalla-host-una-sola-ventana-con-el-audio"),
                    state.source == SourceChoice::App,
                    message(SourceChoice::App),
                ),
            ]
        },
        space::S,
    )
}

fn setup(app: &App) -> Element<'_, Message> {
    let state = &app.host;
    let video = &app.settings.video;
    let (width, height) = video.max_resolution.size();
    let codec = match video.codec {
        CodecChoice::Auto => t!("pantalla-host-el-mejor-comun-con-los-invitados"),
        CodecChoice::Av1 => "AV1",
        CodecChoice::Hevc => "HEVC",
        CodecChoice::H264 => "H.264",
    };
    let cards = source_cards(state);
    let test_source: Element<'_, Message> = if state.source == SourceChoice::Synthetic {
        ui::banner(
            Tone::Info,
            t!("pantalla-host-se-transmitira-la-fuente-de-prueba"),
            Some((
                t!("pantalla-host-volver-a-la-pantalla"),
                on(Msg::Source(SourceChoice::Screen)),
            )),
        )
    } else {
        row![
            ui::button_with(
                Kind::Link,
                t!("pantalla-host-usar-la-fuente-de-prueba"),
                None,
                Some(on(Msg::Source(SourceChoice::Synthetic))),
            ),
            ui::caption(t!("pantalla-host-un-patron-animado-sirve-para-probar")),
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into()
    };
    let source = column![cards, test_source].spacing(space::XS);
    let target = target_section(state);
    let audio = audio_section(app);
    let voice = ui::section(
        t!("pantalla-host-voz"),
        None,
        vec![ui::pref_row(
            t!("pantalla-host-hablar-por-la-sala-con-el"),
            Some(t!("pantalla-host-los-invitados-te-oyen-y-hablan").to_owned()),
            ui::switch(state.voice, Some(|on_off| on(Msg::Voice(on_off)))),
        )],
    );
    let quality = ui::section(
        t!("pantalla-host-calidad"),
        None,
        vec![ui::pref_row(
            t!("pantalla-host-video"),
            Some(t!(
                "pantalla-host-hasta-width-height-a-fps-y",
                width = width,
                height = height,
                fps = video.fps,
                valor = stats::kbps(f64::from(video.max_bitrate_kbps)),
                codec = codec
            )),
            ui::button_with(
                Kind::Link,
                t!("pantalla-host-cambiar"),
                None,
                Some(on(Msg::VideoSettings)),
            ),
        )],
    );
    let mut page = Column::new()
        .spacing(space::L)
        .push(ui::page_header(
            t!("pantalla-host-compartir"),
            Some(t!("pantalla-host-elige-que-ven-y-oyen").to_owned()),
            vec![],
        ))
        .push(source);
    if let Some(target) = target {
        page = page.push(target);
    }
    let mut page = page.push(audio).push(voice).push(quality);
    if let Some(error) = &state.error {
        page = page.push(ui::banner(
            Tone::Error,
            t!("pantalla-host-no-se-pudo-abrir-la-sala", error = error),
            Some((t!("pantalla-host-reintentar"), on(Msg::Start))),
        ));
    }
    let actions: Element<'_, Message> = if state.starting {
        container(ui::progress(
            t!("pantalla-host-abriendo-la-sala-en-linux-elige"),
            None,
            app.now.elapsed().as_secs_f32(),
        ))
        .width(Length::Fixed(420.0))
        .into()
    } else {
        row![
            ui::button_with(
                Kind::Normal,
                t!("app-cancelar"),
                None,
                Some(on(Msg::Cancel)),
            ),
            ui::button_with(
                Kind::Primary,
                t!("pantalla-host-empezar-a-transmitir"),
                None,
                (!state.picking).then(|| on(Msg::Start)),
            ),
        ]
        .spacing(space::S)
        .into()
    };
    page = page.push(row![ui::fill(), actions].align_y(Alignment::Center));
    ui::page(page)
}

/// Monitor o ventana: lo elegido con "Cambiar…" (el selector del sistema) o la lista de aquí.
fn target_section(state: &State) -> Option<Element<'_, Message>> {
    let (title, label) = match state.source {
        SourceChoice::Screen => (t!("pantalla-host-pantalla"), t!("pantalla-host-monitor")),
        SourceChoice::App => (t!("pantalla-host-aplicacion"), t!("pantalla-host-ventana")),
        SourceChoice::Synthetic => return None,
    };
    let mut rows = Vec::new();
    let options = state.current_options();
    if let Some(err) = options.and_then(|options| options.targets_error.as_ref())
        && options.is_some_and(|options| options.targets.is_empty())
    {
        rows.push(ui::pref_row(
            if state.source == SourceChoice::App {
                t!("pantalla-host-no-se-pudieron-listar-las-ventanas-titulo")
            } else {
                t!("pantalla-host-no-se-pudieron-listar-los-monitores-titulo")
            },
            Some(err.clone()),
            ui::gap(0.0),
        ));
    } else if options.is_none() {
        // Aún no se ha mirado, o lo que hay es de la otra fuente.
        rows.push(ui::pref_row(
            t!("pantalla-host-buscando"),
            None,
            ui::gap(0.0),
        ));
    } else if state.system_picks() {
        rows.push(picked_row(state));
    } else {
        let targets: Vec<TargetOption> = options
            .iter()
            .flat_map(|options| options.targets.iter().cloned().map(TargetOption))
            .collect();
        let selected = state.target.clone().map(TargetOption);
        rows.push(ui::pref_row(
            label,
            None,
            ui::select(targets, selected, |target| on(Msg::Target(target))),
        ));
    }
    if state.source == SourceChoice::App {
        rows.push(note(t!("pantalla-host-si-la-ventana-se-ve-en-negro")));
    }
    Some(ui::section(title, None, rows))
}

/// Fila de lo elegido en el selector del sistema, con el botón que lo abre.
fn picked_row(state: &State) -> Element<'_, Message> {
    let app = state.source == SourceChoice::App;
    let (text, subtitle) = match (&state.picked, state.picking) {
        (_, true) => (
            t!("pantalla-host-elige-en-el-selector-del-sistema").to_owned(),
            None,
        ),
        (Some(picked), false) => (picked_label(&picked.target), None),
        (None, false) => (
            if app {
                t!("pantalla-host-aun-no-has-elegido-ventana")
            } else {
                t!("pantalla-host-aun-no-has-elegido-monitor")
            }
            .to_owned(),
            Some(t!("pantalla-host-si-no-eliges-ahora").to_owned()),
        ),
    };
    let action = match (&state.picked, app) {
        (None, _) => t!("pantalla-host-elegir"),
        (Some(_), false) => t!("pantalla-host-cambiar-puntos"),
        (Some(_), true) => t!("pantalla-host-elegir-otra"),
    };
    ui::pref_row(
        text,
        subtitle,
        ui::button_with(
            Kind::Normal,
            action,
            None,
            (!state.picking).then(|| on(Msg::Pick)),
        ),
    )
}

/// Cómo se nombra lo elegido en el selector del sistema (Wayland solo da el tamaño).
fn picked_label(target: &CaptureTarget) -> String {
    match target {
        CaptureTarget::Monitor { name, .. } if name.is_empty() => {
            t!("pantalla-host-monitor").to_owned()
        }
        CaptureTarget::Monitor { name, .. } => {
            t!("pantalla-host-monitor-de-size", size = name.as_str())
        }
        CaptureTarget::Window { title, .. } => {
            t!("pantalla-host-ventana-de-size", size = title.as_str())
        }
    }
}

/// Texto secundario dentro de una sección.
fn note(content: &str) -> Element<'_, Message> {
    container(ui::caption(content))
        .padding(Padding::from([4.0, 16.0]))
        .into()
}

/// Fila de la lista de audio: la casilla, el nombre y la nota a la derecha.
fn audio_row<'a>(
    name: String,
    note_text: &'a str,
    checked: Option<bool>,
    toggle: Option<Message>,
) -> Element<'a, Message> {
    let mark: Element<'a, Message> = match checked {
        Some(checked) => checkbox(checked)
            .on_toggle_maybe(toggle.map(|message| move |_| message.clone()))
            .size(18.0)
            .into(),
        None => icon_with(Icon::CircleCheck, 18.0, SvgClass::Accent),
    };
    let label: Element<'a, Message> = if checked == Some(false) {
        ui::secondary(name).into()
    } else {
        ui::body(name).into()
    };
    row![
        mark,
        container(label).width(Length::Fill),
        ui::caption(note_text)
    ]
    .spacing(space::S)
    .align_y(Alignment::Center)
    .padding(Padding::from([10.0, 16.0]))
    .height(Length::Shrink)
    .into()
}

/// Audio que se envía: las aplicaciones con su casilla.
fn audio_section(app: &App) -> Element<'_, Message> {
    let state = &app.host;
    let unsupported = state
        .current_options()
        .and_then(|options| options.apps.as_ref().err());
    let subtitle = match state.source {
        SourceChoice::App => t!("pantalla-host-marcada-la-aplicacion-que-elegiste"),
        SourceChoice::Screen | SourceChoice::Synthetic => {
            t!("pantalla-host-las-aplicaciones-que-empiecen-a-sonar")
        }
    };
    let mut rows: Vec<Element<'_, Message>> = Vec::new();
    if let Some(why) = unsupported {
        rows.push(ui::pref_row(
            t!("pantalla-host-todo-lo-que-suena"),
            Some(t!(
                "pantalla-host-este-equipo-no-separa-el-audio",
                why = why
            )),
            icon_with(Icon::CircleCheck, 20.0, SvgClass::Accent),
        ));
    } else {
        let audio_rows = state.audio_rows(&app.settings.audio.excluded_apps);
        if audio_rows.is_empty() {
            rows.push(ui::pref_row(
                if state.current_options().is_none() {
                    t!("pantalla-host-buscando")
                } else {
                    t!("pantalla-host-ninguna-aplicacion-suena-ahora")
                },
                None,
                ui::gap(0.0),
            ));
        }
        for row in audio_rows {
            let AudioRow {
                key,
                name,
                checked,
                note,
            } = row;
            let toggle = on(Msg::AppAudio(key, name.clone(), !checked));
            rows.push(audio_row(name, note, Some(checked), Some(toggle)));
        }
    }
    let header = row![
        ui::strong(t!("pantalla-host-audio-que-se-envia")),
        ui::caption(subtitle).wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(space::XS)
    .align_y(Alignment::End);
    column![
        header,
        ui::boxed_list(rows),
        container(ui::caption(t!("pantalla-host-coroom-nunca-se-envia-a-si")))
            .padding(Padding::from([0.0, 4.0])),
    ]
    .spacing(space::XS)
    .into()
}

fn code_card<'a>(room: &'a Room) -> Element<'a, Message> {
    let code: Element<'a, Message> = match &room.code {
        Some(code) => row![
            ui::mono(spaced(code), type_size::CODE).font(coroom_ui::font::MONO),
            ui::fill(),
            ui::button_with(
                Kind::Normal,
                t!("pantalla-host-copiar"),
                Some(Icon::Copy),
                Some(on(Msg::CopyCode))
            )
        ]
        .align_y(Alignment::Center)
        .into(),
        None => ui::progress(
            t!("pantalla-host-creando-la-sala-en-el-servidor"),
            None,
            0.5,
        ),
    };
    ui::card(
        column![
            ui::caption(t!("pantalla-host-codigo-de-la-sala")),
            code,
            ui::secondary(t!("pantalla-host-daselo-a-tus-invitados-lo-escriben"))
                .wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(space::XS),
    )
}

/// `K7Q2MZ` → `K7Q 2MZ` (más fácil de dictar).
pub fn spaced(code: &str) -> String {
    let (a, b) = code.split_at(code.len().min(3));
    if b.is_empty() {
        a.to_owned()
    } else {
        format!("{a} {b}")
    }
}

fn preview_card(room: &Room) -> Element<'_, Message> {
    let body: Element<'_, Message> = if room.preview_unavailable {
        container(
            column![
                icon_with(Icon::Monitor, 28.0, SvgClass::Overlay),
                iced_widget::text(t!("pantalla-host-sin-vista-previa-la-captura-va"))
                    .size(type_size::SECONDARY)
                    .class(coroom_ui::style::TextClass::Overlay)
                    .align_x(iced_core::alignment::Horizontal::Center)
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(space::XS)
            .align_x(Alignment::Center),
        )
        .padding(space::M)
        .center(Length::Fill)
        .into()
    } else {
        video_view(&room.preview)
    };
    container(body)
        .width(Length::Fixed(288.0))
        .height(Length::Fixed(162.0))
        .class(ContainerClass::Video)
        .into()
}

fn guest_card<'a>(app: &'a App, status: &'a GuestStatus) -> Element<'a, Message> {
    let id = status.guest;
    let connected = status.path.is_some();
    let mut facts = Vec::new();
    if let Some(path) = status.path {
        facts.push(match path {
            coroom_transport::Path::Direct => "directo".to_owned(),
            coroom_transport::Path::Relayed => t!("pantalla-host-por-el-relay").to_owned(),
        });
    }
    if let Some(rtt) = status.rtt_ms {
        facts.push(format!("RTT {}", stats::ms(rtt)));
    }
    if let Some(kbps) = status.target_kbps {
        facts.push(stats::kbps(f64::from(kbps)));
    }
    if status.input.samples > 0 {
        facts.push(t!(
            "pantalla-host-entrada",
            valor = stats::ms(status.input.p50_us as f64 / 1000.0)
        ));
    }
    if facts.is_empty() {
        facts.push(t!("pantalla-host-conectando").to_owned());
    }
    let header = row![
        ui::avatar(&app.theme, &status.name, size::AVATAR),
        column![
            ui::strong(status.name.as_str()),
            ui::secondary(facts.join(" · "))
        ]
        .spacing(2)
        .width(Length::Fill),
        if connected {
            ui::status_badge(Tone::Success, t!("pantalla-host-conectado"))
        } else {
            ui::status_badge(Tone::Warning, t!("pantalla-host-conectando-2"))
        },
        ui::icon_button(
            Icon::Fingerprint,
            t!("pantalla-host-numero-de-seguridad"),
            Some(on(Msg::Safety(id)))
        ),
        ui::button_with(
            Kind::DestructiveFlat,
            t!("pantalla-host-expulsar"),
            Some(Icon::UserX),
            Some(on(Msg::Kick(id)))
        )
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    let max = coroom_session::input::max_gamepads();
    let slots = pad_choices(app.settings.input.preferred_slot, max);
    let current = PadSlot(status.permissions.gamepad_slot);
    let permissions = row![
        row![
            icon_with(Icon::Keyboard, 18.0, SvgClass::Secondary),
            ui::body(t!("pantalla-host-teclado-y-raton")),
            ui::switch(
                status.permissions.keyboard_mouse,
                Some(move |on_off| on(Msg::Keyboard(id, on_off)))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
        {
            let mut pad = row![
                icon_with(Icon::Gamepad, 18.0, SvgClass::Secondary),
                ui::select(slots, Some(current), move |slot| on(Msg::Pad(id, slot)))
            ]
            .spacing(space::XS)
            .align_y(Alignment::Center);
            if status.permissions.gamepad_slot.is_some() {
                pad = pad.push(ui::select(
                    vec![PadMode(true), PadMode(false)],
                    Some(PadMode(status.permissions.gamepad_raw)),
                    move |mode| on(Msg::PadMode(id, mode)),
                ));
            }
            pad
        },
        row![
            icon_with(Icon::Mic, 18.0, SvgClass::Secondary),
            ui::body(t!("pantalla-host-voz")),
            ui::switch(
                status.permissions.voice,
                Some(move |on_off| on(Msg::GuestVoice(id, on_off)))
            )
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
    ]
    .spacing(space::L)
    .align_y(Alignment::Center);
    let mut card = column![header, permissions].spacing(space::S);
    if status.permissions.gamepad_slot.is_some() {
        card = card.push(pad_line(status));
    }
    if !status.input_key
        && (status.permissions.keyboard_mouse || status.permissions.gamepad_slot.is_some())
    {
        card = card.push(ui::caption(t!(
            "pantalla-host-esperando-la-clave-de-entrada-pareja"
        )));
    }
    ui::card(card)
}

/// El mando que declara el invitado (nombre, VID:PID y cómo llega) y, si no va tal cual
/// pudiendo, por qué.
fn pad_line(status: &GuestStatus) -> Element<'_, Message> {
    let Some(pad) = &status.gamepad else {
        return ui::caption(t!("pantalla-host-todavia-no-ha-conectado-ningun-mando")).into();
    };
    let transport = match pad.transport {
        PadTransport::Hid => "HID",
        PadTransport::XInput => "XInput",
        PadTransport::Converted => t!("pantalla-host-solo-convertido"),
    };
    let mut line = column![
        row![
            ui::caption(t!("pantalla-host-su-mando-name", name = pad.name.as_str())),
            ui::status_badge(
                Tone::Neutral,
                format!("{:04x}:{:04x} · {transport}", pad.vendor, pad.product)
            ),
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
    ]
    .spacing(2);
    if let Some(note) = &pad.note {
        line = line.push(ui::caption(t!(
            "pantalla-host-va-convertido-note",
            note = note.as_str()
        )));
    }
    line.into()
}

fn room_view<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let status = match (&room.codec, room.guests.is_empty()) {
        (Some(codec), _) => t!("pantalla-host-compartiendo-codec", codec = codec),
        (None, true) => t!("pantalla-host-esperando-al-primer-invitado-para-elegir").to_owned(),
        (None, false) => t!("pantalla-host-eligiendo-el-codec-con-tus-invitados").to_owned(),
    };
    let pause: Element<'a, Message> = if room.paused {
        ui::button_with(
            Kind::Normal,
            t!("pantalla-host-reanudar-la-entrada"),
            Some(Icon::Play),
            Some(on(Msg::Pause(false))),
        )
        .into()
    } else {
        ui::button_with(
            Kind::Normal,
            t!("pantalla-host-pausar-a-todos"),
            Some(Icon::Pause),
            Some(on(Msg::Pause(true))),
        )
        .into()
    };
    let stop: Element<'a, Message> = ui::button_with(
        Kind::Destructive,
        t!("pantalla-host-dejar-de-compartir"),
        Some(Icon::Close),
        (!app.host.stopping).then(|| on(Msg::Stop)),
    )
    .into();
    let mut page = Column::new().spacing(space::L).push(ui::page_header(
        t!("pantalla-host-tu-sala"),
        Some(status),
        vec![pause, stop],
    ));
    if room.paused {
        page = page.push(ui::banner(
            Tone::Warning,
            t!("pantalla-host-la-entrada-de-todos-los-invitados"),
            Some((t!("pantalla-host-reanudar"), on(Msg::Pause(false)))),
        ));
    }
    if let Some(relay) = &room.relay {
        page = page.push(ui::banner(
            Tone::Warning,
            t!(
                "pantalla-host-sin-relay-relay-solo-podran-entrar",
                relay = relay
            ),
            None,
        ));
    }
    page = page.push(
        row![
            container(code_card(room)).width(Length::Fill),
            preview_card(room)
        ]
        .spacing(space::M)
        .align_y(Alignment::Start),
    );
    page = page.push(transmission(app, room));
    let guests: Element<'a, Message> = if room.guests.is_empty() {
        ui::card(ui::empty_state(
            Icon::Users,
            t!("pantalla-host-todavia-no-ha-entrado-nadie"),
            t!("pantalla-host-comparte-el-codigo-de-la-sala"),
            None,
        ))
    } else {
        Column::with_children(room.guests.iter().map(|status| guest_card(app, status)))
            .spacing(space::S)
            .into()
    };
    page = page.push(
        column![
            ui::heading(t!("pantalla-host-invitados", valor = room.guests.len())),
            guests
        ]
        .spacing(space::S),
    );
    page = page.push(invite_friends(app, room));
    ui::page(page)
}

/// Qué se comparte, su audio y el vídeo, con la sala abierta: se cambian sin cerrarla.
fn transmission<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let state = &app.host;
    let mut rows: Vec<Element<'a, Message>> = Vec::new();

    // Qué compartes: pantalla o aplicación, el monitor o la ventana y, si cambió, el botón.
    let kinds = ui::segmented(vec![
        (
            t!("pantalla-host-pantalla").to_owned(),
            on(Msg::Source(SourceChoice::Screen)),
            state.source == SourceChoice::Screen,
        ),
        (
            t!("pantalla-host-aplicacion").to_owned(),
            on(Msg::Source(SourceChoice::App)),
            state.source == SourceChoice::App,
        ),
    ]);
    let options = state.current_options();
    let busy = state.picking || state.switching.is_some();
    let target: Option<Element<'a, Message>> = match state.source {
        SourceChoice::Synthetic => None,
        _ if state.system_picks() => Some(
            ui::button_with(
                Kind::Normal,
                t!("pantalla-host-cambiar-puntos"),
                None,
                (!busy).then(|| on(Msg::Pick)),
            )
            .into(),
        ),
        _ => options
            .filter(|options| !options.targets.is_empty())
            .map(|options| {
                let targets: Vec<TargetOption> =
                    options.targets.iter().cloned().map(TargetOption).collect();
                ui::select(targets, state.target.clone().map(TargetOption), |target| {
                    on(Msg::Target(target))
                })
            }),
    };
    let mut control = row![kinds].spacing(space::S).align_y(Alignment::Center);
    if let Some(target) = target {
        control = control.push(target);
    }
    if state.switching.is_none() && state.source_changed() {
        control = control.push(ui::button_with(
            Kind::Primary,
            t!("pantalla-host-compartir-esto"),
            None,
            (!busy).then(|| on(Msg::ApplySource)),
        ));
    }
    let now = state.shared.as_ref().map(sharing_label).unwrap_or_default();
    let subtitle = match &state.switching {
        Some(switching) => t!(
            "pantalla-host-cambiando-a",
            que = sharing_label(switching).as_str()
        ),
        None if state.source_changed() => t!("pantalla-host-ahora-cambiar", que = now.as_str()),
        None => t!("pantalla-host-ahora", que = now.as_str()),
    };
    rows.push(ui::pref_row(
        t!("pantalla-host-que-compartes"),
        Some(subtitle),
        control,
    ));

    // Audio: las casillas se aplican al momento.
    match state
        .current_options()
        .and_then(|options| options.apps.as_ref().err())
    {
        Some(why) => rows.push(ui::pref_row(
            t!("pantalla-host-audio"),
            Some(t!(
                "pantalla-host-este-equipo-no-separa-el-audio",
                why = why
            )),
            icon_with(Icon::CircleCheck, 20.0, SvgClass::Accent),
        )),
        None => {
            let audio_rows = state.audio_rows(&app.settings.audio.excluded_apps);
            rows.push(ui::pref_row(
                t!("pantalla-host-audio"),
                Some(
                    if audio_rows.is_empty() {
                        t!("pantalla-host-ninguna-aplicacion-suena-ahora")
                    } else {
                        t!("pantalla-host-marca-lo-que-oyen")
                    }
                    .to_owned(),
                ),
                ui::gap(0.0),
            ));
            for AudioRow {
                key,
                name,
                checked,
                note,
            } in audio_rows
            {
                let toggle = on(Msg::AppAudio(key, name.clone(), !checked));
                rows.push(audio_row(name, note, Some(checked), Some(toggle)));
            }
        }
    }

    // Vídeo: códec y bitrate máximo, también en Ajustes.
    let video = &app.settings.video;
    let codecs = settings_screen::codec_choices();
    let codec = codecs.iter().find(|c| c.value == video.codec).cloned();
    let bitrates = settings_screen::bitrate_choices();
    let bitrate = bitrates
        .iter()
        .find(|c| c.value == video.max_bitrate_kbps)
        .cloned();
    let in_use = room.codec.as_ref().map_or_else(
        || t!("pantalla-host-cambiar-de-codec-corta-un-momento").to_owned(),
        |codec| t!("pantalla-host-en-uso-codec", codec = codec.as_str()),
    );
    rows.push(ui::pref_row(
        t!("pantalla-settings-codec-preferido"),
        Some(in_use),
        ui::select(codecs, codec, |c| {
            Message::Settings(settings_screen::Msg::Codec(c))
        }),
    ));
    rows.push(ui::pref_row(
        t!("pantalla-settings-bitrate-maximo"),
        Some(t!("pantalla-host-el-bitrate-se-aplica-ya").to_owned()),
        row![
            ui::select(bitrates, bitrate, |c| {
                Message::Settings(settings_screen::Msg::Bitrate(c))
            }),
            ui::button_with(
                Kind::Link,
                t!("pantalla-host-mas-ajustes-de-video"),
                None,
                Some(on(Msg::VideoSettings)),
            ),
        ]
        .spacing(space::S)
        .align_y(Alignment::Center),
    ));
    ui::section(
        t!("pantalla-host-transmision"),
        Some(t!("pantalla-host-transmision-ayuda").to_owned()),
        rows,
    )
}

/// Cómo se nombra una transmisión («la pantalla», «Ventana: Fatal Fury»…).
fn sharing_label(sharing: &Sharing) -> String {
    let target = sharing
        .picked
        .as_ref()
        .map(|picked| picked_label(&picked.target))
        .or_else(|| {
            sharing.target.as_ref().map(|target| match target {
                CaptureTarget::Monitor { name, .. } if !name.is_empty() => name.clone(),
                CaptureTarget::Window { title, .. } if !title.is_empty() => title.clone(),
                _ => picked_label(target),
            })
        });
    match (sharing.source, target) {
        (SourceChoice::Synthetic, _) => t!("pantalla-host-la-fuente-de-prueba").to_owned(),
        (_, Some(target)) => target,
        (SourceChoice::Screen, None) => t!("pantalla-host-pantalla").to_owned(),
        (SourceChoice::App, None) => t!("pantalla-host-ventana").to_owned(),
    }
}

fn invite_friends<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let body: Element<'a, Message> = match &app.friends {
        Load::Loaded(friends) => {
            let inside: Vec<u64> = room.guests.iter().map(|status| status.user).collect();
            let online: Vec<_> = friends
                .friends
                .iter()
                .filter(|friend| friend.online && !inside.contains(&friend.id))
                .collect();
            if online.is_empty() {
                ui::caption(t!("pantalla-host-ninguno-de-tus-amigos-conectados-esta")).into()
            } else {
                let rows = online
                    .into_iter()
                    .map(|friend| {
                        ui::list_row(
                            Some(ui::avatar(&app.theme, &friend.display_name, 28.0)),
                            friend.display_name.as_str(),
                            Some(format!("@{}", friend.username)),
                            vec![
                                ui::button_with(
                                    Kind::Normal,
                                    t!("pantalla-host-invitar"),
                                    Some(Icon::Mail),
                                    room.code.as_ref().map(|_| on(Msg::Invite(friend.id))),
                                )
                                .into(),
                            ],
                        )
                    })
                    .collect();
                ui::boxed_list(rows)
            }
        }
        Load::Failed(err) => ui::banner(Tone::Error, err.as_str(), None),
        Load::Idle | Load::Loading => ui::skeleton(t!("pantalla-host-cargando-tus-amigos"), 1),
    };
    column![ui::heading(t!("pantalla-host-invitar-amigos")), body]
        .spacing(space::S)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;

    fn status(id: u32, name: &str) -> GuestStatus {
        GuestStatus {
            guest: GuestId(id),
            user: u64::from(id) + 10,
            name: name.into(),
            public_key: [0; 32],
            fingerprint: "ab".repeat(32),
            safety_number: coroom_transport::crypto::SafetyNumber::between(&[1; 32], &[2; 32]),
            permissions: Permissions::VIEW_ONLY,
            leaf: id,
            path: None,
            rtt_ms: None,
            target_kbps: None,
            input_key: true,
            input: Default::default(),
            gamepad: None,
        }
    }

    fn open_room(app: &mut App) {
        app.update(Message::Host(Msg::Start));
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "K7Q2MZ".into(),
            fingerprint: "cd".repeat(32),
        })));
        app.update(Message::Event(Event::HostGuests(vec![
            status(1, "Bea"),
            status(2, "Carla"),
        ])));
    }

    fn playing(keys: &[&str]) -> Vec<PlayingApp> {
        keys.iter()
            .map(|key| PlayingApp::new(key, key.trim_end_matches(".exe"), Vec::new()))
            .collect()
    }

    fn window(id: u64, title: &str, app: Option<&str>) -> CaptureTarget {
        CaptureTarget::Window {
            id,
            title: title.into(),
            app: app.map(str::to_owned),
        }
    }

    #[test]
    fn sharing_the_screen_sends_every_app_but_the_removed_ones() {
        let mut app = app();
        app.screen = crate::ui::app::Screen::Main;
        assert_eq!(
            app.update(Message::Navigate(Page::Host)),
            [Effect::LoadShareOptions(SourceChoice::Screen)]
        );
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(playing(&["juego.exe", "discord.exe"])),
        })));
        let rows = app.host.audio_rows(&app.settings.audio.excluded_apps);
        assert!(rows.iter().all(|row| row.checked), "todas marcadas");
        let effects = app.update(Message::Host(Msg::AppAudio(
            "Discord.exe".into(),
            "Discord".into(),
            false,
        )));
        assert_eq!(effects, [Effect::SaveSettings], "se recuerda");
        assert_eq!(app.settings.audio.excluded_apps[0].key, "discord.exe");
        // Discord deja de sonar: sigue en la lista, quitado y recordado.
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(playing(&["juego.exe"])),
        })));
        let rows = app.host.audio_rows(&app.settings.audio.excluded_apps);
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|row| row.key == "discord.exe" && !row.checked)
        );
        app.update(Message::Host(Msg::Voice(true)));
        let effects = app.update(Message::Host(Msg::Start));
        assert_eq!(
            effects,
            [Effect::StartHost(HostRequest {
                source: SourceChoice::Screen,
                share: ShareChoice::default(),
                audio: GameCapture::Apps(AppFilter::AllExcept(vec!["discord.exe".into()])),
                voice: true,
            })],
            "la sala de espera sigue hasta que la sala abra"
        );
        assert!(app.host.starting && app.host.is_open());
        assert!(
            app.update(Message::Host(Msg::Start)).is_empty(),
            "una sola sala"
        );
        app.update(Message::Event(Event::HostStarted(Err(
            "no hay pantalla".into()
        ))));
        assert!(!app.host.is_open());
        assert_eq!(app.host.error.as_deref(), Some("no hay pantalla"));
    }

    #[test]
    fn sharing_an_app_sends_only_its_audio() {
        let mut app = app();
        app.update(Message::Navigate(Page::Host));
        assert_eq!(
            app.update(Message::Host(Msg::Source(SourceChoice::App))),
            [Effect::LoadShareOptions(SourceChoice::App)]
        );
        // Con ventanas que dicen su aplicación (Windows): se marca la de la ventana.
        let game = window(7, "Juego", Some("juego.exe"));
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: vec![game.clone(), window(8, "Navegador", Some("firefox.exe"))],
            targets_error: None,
            apps: Ok(playing(&["discord.exe", "firefox.exe"])),
        })));
        assert_eq!(app.host.target, Some(game.clone()));
        let rows = app.host.audio_rows(&[]);
        assert_eq!(rows[0].key, "juego.exe", "aún sin sonido, pero marcada");
        assert!(rows[0].checked);
        assert!(rows[1..].iter().all(|row| !row.checked));
        app.update(Message::Host(Msg::Target(TargetOption(window(
            8,
            "Navegador",
            Some("firefox.exe"),
        )))));
        assert_eq!(app.host.only, ["firefox.exe"]);
        app.update(Message::Host(Msg::Target(TargetOption(game.clone()))));
        app.update(Message::Host(Msg::AppAudio(
            "discord.exe".into(),
            "discord".into(),
            true,
        )));
        let effects = app.update(Message::Host(Msg::Start));
        assert_eq!(
            effects[0],
            Effect::StartHost(HostRequest {
                source: SourceChoice::App,
                share: ShareChoice {
                    kind: ShareKind::App,
                    target: Some(game),
                    restore_token: None,
                },
                audio: GameCapture::Apps(AppFilter::Only(vec![
                    "juego.exe".into(),
                    "discord.exe".into()
                ])),
                voice: app.host.voice,
            })
        );
    }

    #[test]
    fn without_window_owners_the_likely_game_is_marked() {
        let mut app = app();
        app.update(Message::Host(Msg::Source(SourceChoice::App)));
        // El sistema elige la ventana (Linux): se adivina el juego entre lo que suena.
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(playing(&["discord", "firefox", "juego"])),
        })));
        assert_eq!(app.host.only, ["juego"]);
        app.update(Message::Host(Msg::AppAudio(
            "juego".into(),
            "juego".into(),
            false,
        )));
        assert!(app.host.only.is_empty());
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(playing(&["discord", "juego", "otro"])),
        })));
        assert!(
            app.host.only.is_empty(),
            "lo que elige el usuario se respeta"
        );
        assert_eq!(
            app.host.game_capture(&[]),
            GameCapture::Apps(AppFilter::Only(Vec::new())),
            "sin nada marcado no suena nada, pero se puede marcar con la sala abierta"
        );
        // Sin audio por aplicación en el equipo: todo lo que suena.
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: Vec::new(),
            targets_error: None,
            apps: Err("no".into()),
        })));
        assert_eq!(app.host.game_capture(&[]), GameCapture::System);
    }

    #[test]
    fn the_system_picker_choice_is_shown_and_reused_when_starting() {
        let mut app = app();
        app.update(Message::Navigate(Page::Host));
        app.update(Message::Host(Msg::Source(SourceChoice::Screen)));
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(Vec::new()),
        })));
        assert!(app.host.system_picks());
        assert_eq!(
            app.update(Message::Host(Msg::Pick)),
            [Effect::PickShare(SourceChoice::Screen)]
        );
        assert!(
            app.update(Message::Host(Msg::Pick)).is_empty(),
            "un solo selector a la vez"
        );
        let monitor = CaptureTarget::Monitor {
            id: 0,
            name: "2560 × 1440".to_owned(),
        };
        app.update(Message::Event(Event::SharePicked(
            SourceChoice::Screen,
            Ok(Some(PickedShare {
                target: monitor.clone(),
                restore_token: Some("token".to_owned()),
            })),
        )));
        assert!(!app.host.picking);
        assert_eq!(picked_label(&monitor), "Monitor · 2560 × 1440");
        let effects = app.update(Message::Host(Msg::Start));
        let Effect::StartHost(request) = &effects[0] else {
            panic!("se esperaba abrir la sala: {effects:?}");
        };
        assert_eq!(request.share.target, Some(monitor));
        assert_eq!(request.share.restore_token.as_deref(), Some("token"));
    }

    #[test]
    fn changing_the_source_forgets_the_picked_target() {
        let mut app = app();
        app.update(Message::Navigate(Page::Host));
        app.host.picked = Some(PickedShare {
            target: CaptureTarget::Monitor {
                id: 0,
                name: String::new(),
            },
            restore_token: None,
        });
        app.update(Message::Host(Msg::Source(SourceChoice::App)));
        assert_eq!(app.host.picked, None);
        app.update(Message::Event(Event::SharePicked(
            SourceChoice::Screen,
            Ok(Some(PickedShare {
                target: CaptureTarget::Monitor {
                    id: 0,
                    name: String::new(),
                },
                restore_token: None,
            })),
        )));
        assert_eq!(
            app.host.picked, None,
            "una respuesta para otra fuente no vale"
        );
    }

    #[test]
    fn permissions_are_switches_and_slots_are_exclusive() {
        let mut app = app();
        open_room(&mut app);
        let effects = app.update(Message::Host(Msg::Keyboard(GuestId(1), true)));
        assert!(matches!(
            &effects[..],
            [Effect::SetPermissions(GuestId(1), p)] if p.keyboard_mouse && p.gamepad_slot.is_none()
        ));
        let effects = app.update(Message::Host(Msg::Pad(GuestId(1), PadSlot(Some(2)))));
        assert!(
            matches!(&effects[..], [Effect::SetPermissions(_, p)] if p.gamepad_slot == Some(2) && p.keyboard_mouse)
        );
        // La ranura 2 ya es de Bea.
        assert!(
            app.update(Message::Host(Msg::Pad(GuestId(2), PadSlot(Some(2)))))
                .is_empty()
        );
        assert!(app.toasts.iter().any(|t| t.text.contains("Ranura 2")));
        app.update(Message::Host(Msg::Kick(GuestId(2))));
        assert!(matches!(&app.dialog, Some(Dialog::Kick { name, .. }) if name == "Carla"));
        assert_eq!(
            app.update(Message::ConfirmDialog),
            [Effect::Kick(GuestId(2))]
        );
        assert_eq!(
            app.update(Message::Host(Msg::Pause(true))),
            [Effect::PauseInput(true)]
        );
    }

    #[test]
    fn guests_joining_and_leaving_are_announced() {
        let mut app = app();
        open_room(&mut app);
        let effects = app.update(Message::Event(Event::Host(SessionEvent::GuestJoined {
            guest: GuestId(3),
            user: 13,
            name: "Dani".into(),
            fingerprint: "ef".repeat(32),
            safety_number: coroom_transport::crypto::SafetyNumber::between(&[1; 32], &[3; 32]),
            permissions: Permissions::VIEW_ONLY,
        })));
        assert_eq!(
            effects,
            [Effect::SaveSettings],
            "se recuerda su número de seguridad"
        );
        assert_eq!(app.settings.peers[0].name, "Dani");
        app.update(Message::Event(Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(1),
            kicked: true,
        })));
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text.contains("Bea ha sido expulsado"))
        );
        assert_eq!(app.host.room.as_ref().map(|r| r.guests.len()), Some(1));
        let effects = app.update(Message::Event(Event::HostEnded(None)));
        assert!(app.host.room.is_none());
        assert!(effects.is_empty(), "sin cuenta no hay sala de espera");
    }

    /// Sin haber mirado nunca (se llegó a la página sin `Navigate`), el `Tick` pide la lista; y
    /// el plazo que espera la ventana nunca queda en el pasado tras un `Tick` (sin bucle de
    /// redibujado).
    #[test]
    fn the_tick_refreshes_when_never_refreshed_without_busy_looping() {
        let mut app = app();
        app.screen = crate::ui::app::Screen::Main;
        app.page = Page::Host;
        assert!(app.host.refreshed_at.is_none());
        assert_eq!(app.next_tick(), Some(app.now), "toca ya");
        assert_eq!(
            app.update(Message::Tick),
            [Effect::LoadShareOptions(SourceChoice::Screen)]
        );
        assert_eq!(app.next_tick(), None, "mirando: nada que esperar");
        assert!(app.update(Message::Tick).is_empty(), "una sola petición");
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(Vec::new()),
        })));
        let next = app.next_tick().expect("siguiente vistazo");
        assert_eq!(next, app.now + REFRESH);
        assert!(app.update(Message::Tick).is_empty(), "aún no toca");
        app.now = next;
        assert_eq!(
            app.update(Message::Tick),
            [Effect::LoadShareOptions(SourceChoice::Screen)]
        );
        // Fuera de la barra lateral no se mira ni se espera.
        app.host.refreshing = false;
        app.screen = crate::ui::app::Screen::Login;
        assert!(app.update(Message::Tick).is_empty());
        assert_eq!(app.next_tick(), None);
    }

    /// Lo que llega para la otra fuente no se muestra como una lista vacía: se sigue buscando.
    #[test]
    fn options_for_another_source_are_still_searching() {
        let mut app = app();
        app.screen = crate::ui::app::Screen::Main;
        app.update(Message::Navigate(Page::Host));
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: vec![CaptureTarget::Monitor {
                id: 1,
                name: "Monitor".into(),
            }],
            targets_error: None,
            apps: Ok(Vec::new()),
        })));
        app.update(Message::Host(Msg::Source(SourceChoice::App)));
        assert!(
            app.host.current_options().is_none(),
            "las de la pantalla no valen"
        );
        assert!(!app.host.system_picks());
        // La respuesta tardía de la pantalla tampoco, y se vuelve a pedir la de la app.
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(Vec::new()),
        })));
        assert!(app.host.current_options().is_none());
        assert_eq!(
            app.update(Message::Tick),
            [Effect::LoadShareOptions(SourceChoice::App)]
        );
    }

    /// Si no se pueden listar los monitores, se avisa una vez y no se finge el selector del
    /// sistema.
    #[test]
    fn a_failed_enumeration_is_explained_once() {
        let mut app = app();
        app.screen = crate::ui::app::Screen::Main;
        app.update(Message::Navigate(Page::Host));
        let failed = || {
            Message::Event(Event::ShareOptions(ShareOptions {
                source: SourceChoice::Screen,
                targets: Vec::new(),
                targets_error: Some("sin acceso a la pantalla".into()),
                apps: Ok(Vec::new()),
            }))
        };
        app.update(failed());
        app.update(failed());
        let warnings = app
            .toasts
            .iter()
            .filter(|t| t.text.contains("sin acceso a la pantalla"))
            .count();
        assert_eq!(warnings, 1, "{:?}", app.toasts);
        assert!(!app.host.system_picks());
    }

    /// La sala de espera se cierra cuando la sala ya está abierta, no antes.
    #[test]
    fn the_lobby_stops_once_the_room_is_open() {
        let mut app = app();
        let effects = app.update(Message::Host(Msg::Start));
        assert!(!effects.contains(&Effect::StopLobby), "{effects:?}");
        assert_eq!(
            app.update(Message::Event(Event::HostStarted(
                Ok(VideoFeed::detached())
            ))),
            [Effect::StopLobby]
        );
    }

    #[test]
    fn pad_slots_offer_the_preferred_first() {
        let names: Vec<String> = pad_choices(3, 4).iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            ["Sin mando", "Ranura 3", "Ranura 1", "Ranura 2", "Ranura 4"]
        );
        assert_eq!(pad_choices(0, 2).len(), 3);
        assert_eq!(spaced("K7Q2MZ"), "K7Q 2MZ");
    }

    #[test]
    fn with_the_room_open_the_source_audio_and_video_change_live() {
        let mut app = app();
        app.screen = crate::ui::app::Screen::Main;
        app.update(Message::Host(Msg::Source(SourceChoice::App)));
        let game = window(1, "Fatal Fury", Some("fatalfury.exe"));
        let other = window(2, "Otro juego", Some("otro.exe"));
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: vec![game.clone(), other.clone()],
            targets_error: None,
            apps: Ok(playing(&["fatalfury.exe", "otro.exe", "discord.exe"])),
        })));
        open_room(&mut app);
        assert!(!app.host.source_changed());

        // Audio: marcar una aplicación más se aplica ya a la sala.
        let effects = app.update(Message::Host(Msg::AppAudio(
            "discord.exe".into(),
            "discord".into(),
            true,
        )));
        assert_eq!(
            effects,
            [Effect::SetAppAudio(GameCapture::Apps(AppFilter::Only(
                vec!["fatalfury.exe".into(), "discord.exe".into()]
            )))]
        );

        // Fuente: elegir otra ventana no cambia nada hasta «Compartir esto».
        assert!(
            app.update(Message::Host(Msg::Target(TargetOption(other.clone()))))
                .is_empty()
        );
        assert!(app.host.source_changed());
        let effects = app.update(Message::Host(Msg::ApplySource));
        let [Effect::SwitchSource(request)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(request.share.target, Some(other.clone()));
        assert_eq!(
            request.audio,
            GameCapture::Apps(AppFilter::Only(vec!["otro.exe".into()])),
            "el audio de la ventana nueva va con ella"
        );
        assert!(
            app.update(Message::Host(Msg::ApplySource)).is_empty(),
            "una vez cada vez"
        );
        app.update(Message::Event(Event::SourceSwitched(Ok(()))));
        assert!(!app.host.source_changed());
        assert_eq!(
            app.host.shared.as_ref().and_then(|s| s.target.clone()),
            Some(other)
        );

        // Si no arranca, sigue lo de antes y se avisa.
        app.update(Message::Host(Msg::Target(TargetOption(game))));
        app.update(Message::Host(Msg::ApplySource));
        app.update(Message::Event(Event::SourceSwitched(Err("no".into()))));
        assert!(app.host.switching.is_none());
        assert!(
            app.host.source_changed(),
            "lo elegido sigue sin transmitirse"
        );

        // Vídeo: el bitrate de la sala se aplica ya (y se guarda).
        let bitrate = settings_screen::bitrate_choices()[0].clone();
        let effects = app.update(Message::Settings(settings_screen::Msg::Bitrate(bitrate)));
        assert_eq!(effects, [Effect::SaveSettings, Effect::SetVideo]);
    }
}
