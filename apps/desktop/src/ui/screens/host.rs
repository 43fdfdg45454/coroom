//! Pantalla «Compartir» (anfitrión), según el boceto aprobado.
//!
//! Antes de abrir la sala: arriba, **Qué compartir** (Pantalla, Ventana o Juego; Juego es una
//! ventana con el audio solo de su aplicación) con el monitor o la ventana, y **A quién invitar**
//! con la lista de amigos en vivo (los marcados se invitan en cuanto el servidor crea la sala);
//! debajo, la **calidad** en tres perfiles ([`QualityPreset`]) y **Avanzado** plegado (resolución,
//! fps, códec y codificador, tope del bitrate Auto, 4:4:4 y HDR), cada fila con «En vivo» o «Al
//! abrir la sala» según [`Video::when`]; el audio que se envía y la voz; y «Abrir la sala e
//! invitar». La vista previa no existe hasta abrir la sala (la captura arranca con ella): hasta
//! entonces, un marcador. Donde el sistema elige el monitor o la ventana con su propio selector
//! (el portal en Wayland), «Elegir…» lo abre ya y la elección se repite al abrir con su token;
//! donde no, se elige aquí de la lista. La lista de audio son las aplicaciones que suenan,
//! refrescadas cada [`REFRESH`].
//!
//! Con la sala abierta (la misma página): la cabecera con el código, los invitados y la ruta; la
//! fuente con su vista previa y «Cambiar fuente» (el mismo selector, sin cerrar la sala); los
//! invitados con sus permisos como conmutadores (teclado y ratón, mando, voz), expulsar y número
//! de seguridad; la calidad en vivo; y, a la derecha, el chat de la sala.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_session::audio::GameCapture;
use coroom_session::audio::apps::{AppFilter, PlayingApp, normalize};
use coroom_session::capture::{CaptureTarget, PickedShare, ShareChoice, ShareKind};
use coroom_session::remote::{
    CandidateOutcome, GuestStatus, PadTransport, QualityLevel, SessionEvent, UNLIMITED_KBPS,
    permissions_label, start_kbps,
};
use coroom_session::{GuestId, Permissions};
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass, TextClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, Row, button, checkbox, column, container, row, rule};

use super::chat;
use crate::ui::app::{App, Dialog, Effect, Event, HostRequest, Load, Message, Page, quality_tone};
use crate::ui::settings::{
    BITRATE_CHOICES, CodecChoice, EncoderChoice, FPS_CHOICES, KnownPeer, QualityPreset,
    RememberedApp, Resolution, Video, VideoOption, When,
};
use crate::ui::stats;
use crate::ui::video::{VideoFeed, video_view};

/// Cada cuánto se vuelve a mirar qué suena y qué se puede compartir.
pub const REFRESH: Duration = Duration::from_secs(2);

/// Aplicaciones que casi nunca son el juego: no se marcan solas al compartir un juego cuando el
/// sistema no dice de quién es la ventana.
const NOT_GAMES: [&str; 16] = [
    "firefox", "chrome", "chromium", "brave", "msedge", "discord", "vesktop", "steam", "spotify",
    "telegram", "slack", "teams", "zoom", "obs", "signal", "webcord",
];

/// Ancho del panel del chat con la sala abierta.
const CHAT_WIDTH: f32 = 320.0;

/// Ancho de la miniatura de la fuente antes de abrir la sala (16:9).
const THUMB_WIDTH: f32 = 160.0;

/// Ancho de la vista previa con la sala abierta (16:9).
const PREVIEW_WIDTH: f32 = 192.0;

/// Caracteres de un monitor o una ventana en su desplegable: los títulos de ventana de Windows
/// son largos y el desplegable va al lado de la miniatura.
const TARGET_LABEL_CHARS: usize = 34;

/// Fuente de la captura (lo que el backend sabe abrir).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceChoice {
    /// Un monitor entero.
    #[default]
    Screen,
    /// Una ventana.
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

/// Qué se comparte, como lo elige el anfitrión en el segmentado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShareMode {
    /// Un monitor, con el audio de todas las aplicaciones salvo las quitadas.
    #[default]
    Screen,
    /// Una ventana, con el audio de todas las aplicaciones salvo las quitadas.
    Window,
    /// Una ventana, con el audio solo de su aplicación (y las que se marquen).
    Game,
    /// La fuente de prueba (un patrón animado).
    Test,
}

impl ShareMode {
    /// Los del segmentado (la fuente de prueba va aparte, como enlace).
    pub const PICKER: [ShareMode; 3] = [ShareMode::Screen, ShareMode::Window, ShareMode::Game];

    /// Fuente que se captura.
    pub fn source(self) -> SourceChoice {
        match self {
            ShareMode::Screen => SourceChoice::Screen,
            ShareMode::Window | ShareMode::Game => SourceChoice::App,
            ShareMode::Test => SourceChoice::Synthetic,
        }
    }

    /// Si solo suena la aplicación elegida (y las marcadas a mano).
    fn only_its_audio(self) -> bool {
        self == ShareMode::Game
    }

    fn label(self) -> &'static str {
        match self {
            ShareMode::Screen => t!("host-modo-pantalla"),
            ShareMode::Window => t!("host-modo-ventana"),
            ShareMode::Game => t!("host-modo-juego"),
            ShareMode::Test => t!("host-la-fuente-de-prueba"),
        }
    }

    fn help(self) -> &'static str {
        match self {
            ShareMode::Screen => t!("host-pantalla-ayuda"),
            ShareMode::Window => t!("host-ventana-ayuda"),
            ShareMode::Game => t!("host-juego-ayuda"),
            ShareMode::Test => t!("host-fuente-de-prueba-ayuda"),
        }
    }

    fn icon(self) -> Icon {
        match self {
            ShareMode::Screen => Icon::Monitor,
            ShareMode::Window => Icon::AppWindow,
            ShareMode::Game => Icon::Gamepad,
            ShareMode::Test => Icon::Activity,
        }
    }
}

/// Monitor o ventana en el selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetOption(pub CaptureTarget);

impl std::fmt::Display for TargetOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match &self.0 {
            CaptureTarget::Monitor { name, .. } => name,
            CaptureTarget::Window { title, .. } => title,
        };
        f.write_str(&ui::ellipsize(label, TARGET_LABEL_CHARS))
    }
}

/// Opción de un desplegable con su texto.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick<T> {
    /// Valor.
    pub value: T,
    /// Texto.
    pub label: String,
}

impl<T> std::fmt::Display for Pick<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn pick<T>(value: T, label: impl Into<String>) -> Pick<T> {
    Pick {
        value,
        label: label.into(),
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
            None => f.write_str(t!("host-sin-mando")),
            Some(slot) => write!(f, "{}", t!("host-ranura-slot", slot = slot)),
        }
    }
}

/// Cómo llega el mando de un invitado: tal cual (`true`) o convertido a Xbox 360.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadMode(pub bool);

impl std::fmt::Display for PadMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0 {
            t!("host-tal-cual")
        } else {
            t!("host-convertido-a-xbox-360")
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
    /// Códec en uso y con qué se codifica («AV1 por SVT-AV1»).
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
    /// Invitados cuya voz no se oye en este equipo (decisión del anfitrión, solo para él).
    pub muted: BTreeSet<GuestId>,
    /// Si los invitados de esta sala entran callados.
    pub guests_muted: bool,
}

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Qué se comparte (pantalla, ventana, juego o la fuente de prueba).
    pub mode: ShareMode,
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
    pub refreshed_at: Option<Instant>,
    /// Al compartir un juego: las aplicaciones cuyo audio se envía.
    pub only: Vec<String>,
    /// Si [`State::only`] lo eligió la aplicación (se rehace al cambiar de ventana o de lo que
    /// suena) o el usuario.
    pub only_chosen: bool,
    /// Lista de audio desplegada.
    pub audio_open: bool,
    /// Amigos marcados para invitar al abrir la sala.
    pub invite: BTreeSet<u64>,
    /// Cuándo llegó la última lista de amigos o un cambio de presencia.
    pub friends_at: Option<Instant>,
    /// «Avanzado» desplegado.
    pub advanced: bool,
    /// El último tope del bitrate elegido (para volver a él al marcar «Tope»).
    pub last_cap_kbps: Option<u32>,
    /// Abriendo.
    pub starting: bool,
    /// Cerrando.
    pub stopping: bool,
    /// Error al abrir.
    pub error: Option<String>,
    /// Sala abierta.
    pub room: Option<Room>,
    /// Con la sala abierta, el selector de fuente desplegado («Cambiar fuente»).
    pub changing_source: bool,
    /// Lo que se transmite desde que se abrió la sala (o desde el último cambio).
    pub shared: Option<Sharing>,
    /// Cambiando lo que se comparte con la sala abierta: lo pedido.
    pub switching: Option<Sharing>,
}

/// Fuente y monitor o ventana de una transmisión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharing {
    /// Cómo se eligió.
    pub mode: ShareMode,
    /// Monitor o ventana de la lista.
    pub target: Option<CaptureTarget>,
    /// Lo elegido en el selector del sistema.
    pub picked: Option<PickedShare>,
}

impl Sharing {
    /// Si se captura lo mismo (Ventana y Juego solo cambian el audio; el título de una ventana
    /// puede cambiar sin que cambie la ventana).
    fn same(&self, other: &Sharing) -> bool {
        let targets = match (&self.target, &other.target) {
            (Some(a), Some(b)) => same_target(a, b),
            (None, None) => true,
            _ => false,
        };
        self.mode.source() == other.mode.source() && targets && self.picked == other.picked
    }
}

impl State {
    /// Si hay una sala abierta (o abriéndose).
    pub fn is_open(&self) -> bool {
        self.room.is_some() || self.starting
    }

    /// Fuente que se captura.
    pub fn source(&self) -> SourceChoice {
        self.mode.source()
    }

    /// Vista previa, si hay sala.
    pub fn preview(&self) -> Option<Arc<VideoFeed>> {
        self.room.as_ref().map(|room| Arc::clone(&room.preview))
    }

    /// Cuándo toca volver a mirar qué se puede compartir (antes de abrir la sala y con ella
    /// abierta, para cambiar de fuente): `now` si nunca se ha mirado, `None` mientras se abre o se
    /// mira. Es el plazo que espera la ventana; [`State::refresh_due`] dice si ya llegó.
    pub fn next_refresh(&self, now: Instant) -> Option<Instant> {
        if self.starting || self.refreshing {
            return None;
        }
        Some(self.refreshed_at.map_or(now, |at| at + REFRESH))
    }

    /// Si toca volver a mirar qué se puede compartir.
    pub fn refresh_due(&self, now: Instant) -> bool {
        !self.starting && !self.refreshing && self.refreshed_at.is_none_or(|at| now >= at + REFRESH)
    }

    /// Lo elegido ahora en la pantalla.
    pub fn sharing(&self) -> Sharing {
        Sharing {
            mode: self.mode,
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
            kind: self.source().kind(),
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
            .filter(|options| options.source == self.source())
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

    /// El juego más probable: la aplicación de la ventana o, si el sistema no la dice, la primera
    /// que suena y no parece un navegador, un chat o una tienda.
    fn likely_game(&self) -> Option<String> {
        self.target_app().map(normalize).or_else(|| {
            self.playing()?
                .iter()
                .find(|app| !NOT_GAMES.iter().any(|word| app.key.contains(word)))
                .map(|app| app.key.clone())
        })
    }

    /// Rehace la selección automática del audio del juego.
    fn auto_select(&mut self) {
        // Con la sala abierta, el audio de lo que ya se transmite no cambia solo.
        let live = self.room.is_some() && !self.source_changed();
        if self.only_chosen || !self.mode.only_its_audio() || live {
            return;
        }
        self.only = self.likely_game().into_iter().collect();
    }

    /// Filas de la lista de audio, con las aplicaciones quitadas que se recuerdan.
    pub fn audio_rows(&self, excluded: &[RememberedApp]) -> Vec<AudioRow> {
        let playing = self.playing().unwrap_or_default();
        let is_excluded = |key: &str| excluded.iter().any(|app| normalize(&app.key) == key);
        let mut rows: Vec<AudioRow> = Vec::new();
        if self.mode.only_its_audio() {
            let chosen = self.target_app().map(normalize);
            if let Some(key) = &chosen
                && !playing.iter().any(|app| &app.key == key)
            {
                rows.push(AudioRow {
                    key: key.clone(),
                    name: key.trim_end_matches(".exe").to_owned(),
                    checked: self.only.contains(key),
                    note: t!("host-la-aplicacion-elegida-aun-sin-sonido"),
                });
            }
            rows.extend(playing.iter().map(|app| AudioRow {
                key: app.key.clone(),
                name: app.name.clone(),
                checked: self.only.contains(&app.key),
                note: if chosen.as_ref() == Some(&app.key) {
                    t!("host-la-aplicacion-elegida")
                } else {
                    t!("host-sonando")
                },
            }));
        } else {
            rows.extend(playing.iter().map(|app| {
                let out = is_excluded(&app.key);
                AudioRow {
                    key: app.key.clone(),
                    name: app.name.clone(),
                    checked: !out,
                    note: if out {
                        t!("host-quitada-siempre")
                    } else {
                        t!("host-sonando")
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
                        note: t!("host-quitada-siempre"),
                    }),
            );
        }
        rows
    }

    /// Audio del juego que se pide al abrir la sala.
    pub fn game_capture(&self, excluded: &[RememberedApp]) -> GameCapture {
        if self.playing().is_none() {
            // Sin audio por aplicación en este equipo: todo lo que suena.
            return GameCapture::System;
        }
        if self.mode.only_its_audio() {
            // Aunque no se marque ninguna, la captura por aplicación arranca: así se puede marcar
            // una con la sala abierta.
            GameCapture::Apps(AppFilter::Only(self.only.clone()))
        } else {
            GameCapture::Apps(AppFilter::AllExcept(
                excluded.iter().map(|app| normalize(&app.key)).collect(),
            ))
        }
    }
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Qué se comparte.
    Mode(ShareMode),
    /// Monitor o ventana.
    Target(TargetOption),
    /// Enviar o no el audio de una aplicación (clave, nombre).
    AppAudio(String, String, bool),
    /// Desplegar o plegar la lista de audio.
    AudioOpen(bool),
    /// Voz del anfitrión.
    Voice(bool),
    /// Los invitados entran callados (en esta sala).
    GuestsMuted(bool),
    /// Abrir el selector del sistema para elegir el monitor o la ventana.
    Pick,
    /// Marcar o desmarcar un amigo para invitarlo al abrir.
    Mark(u64, bool),
    /// Volver a pedir la lista de amigos.
    ReloadFriends,
    /// Elegir un perfil de calidad.
    Preset(QualityPreset),
    /// Desplegar o plegar «Avanzado».
    Advanced(bool),
    /// Resolución máxima.
    Resolution(Pick<Resolution>),
    /// Fotogramas por segundo.
    Fps(u32),
    /// Códec.
    Codec(Pick<CodecChoice>),
    /// Codificador.
    Encoder(Pick<EncoderChoice>),
    /// Poner o quitar el tope del bitrate Auto.
    Capped(bool),
    /// Tope del bitrate Auto (kbit/s).
    Cap(Pick<u32>),
    /// Croma 4:4:4.
    Chroma444(bool),
    /// HDR.
    Hdr(bool),
    /// Abrir la sala.
    Start,
    /// Copiar el código.
    CopyCode,
    /// Cerrar la sala (pide confirmación).
    Stop,
    /// Pausar o reanudar la entrada de todos.
    Pause(bool),
    /// Teclado y ratón de un invitado.
    Keyboard(GuestId, bool),
    /// Dar o quitar un mando a un invitado (la primera ranura libre).
    PadOn(GuestId, bool),
    /// Ranura del mando de un invitado.
    Pad(GuestId, PadSlot),
    /// Si el mando de un invitado va tal cual o convertido.
    PadMode(GuestId, PadMode),
    /// Voz de un invitado.
    GuestVoice(GuestId, bool),
    /// Oír o no la voz de un invitado en este equipo.
    Hear(GuestId, bool),
    /// Expulsar (pide confirmación).
    Kick(GuestId),
    /// Número de seguridad de un invitado.
    Safety(GuestId),
    /// Invitar a un amigo conectado con la sala abierta.
    Invite(u64),
    /// Con la sala abierta: desplegar (o cancelar) el selector de fuente.
    ChangeSource(bool),
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

/// Si la ranura `slot` es de otro invitado que `id`.
fn slot_taken(app: &App, id: GuestId, slot: u8) -> bool {
    app.host.room.as_ref().is_some_and(|room| {
        room.guests
            .iter()
            .any(|g| g.guest != id && g.permissions.gamepad_slot == Some(slot))
    })
}

/// Qué opciones de vídeo cambian de `before` a `after`.
fn changed_options(before: &Video, after: &Video) -> Vec<VideoOption> {
    let mut changed = Vec::new();
    if before.max_resolution != after.max_resolution {
        changed.push(VideoOption::Resolution);
    }
    if before.fps != after.fps {
        changed.push(VideoOption::Fps);
    }
    if before.codec != after.codec || before.encoder != after.encoder {
        changed.push(VideoOption::Codec);
    }
    if before.max_bitrate_kbps != after.max_bitrate_kbps {
        changed.push(VideoOption::Bitrate);
    }
    if before.chroma_444 != after.chroma_444 {
        changed.push(VideoOption::Chroma444);
    }
    if before.hdr != after.hdr {
        changed.push(VideoOption::Hdr);
    }
    changed
}

/// Cambia el vídeo de los ajustes y lo guarda; con la sala abierta, aplica lo que va en vivo
/// ([`Video::when`]) y avisa de lo que espera a la próxima sala. `custom`: el cambio viene de
/// «Avanzado» y el perfil pasa a «Personalizado».
fn change_video(app: &mut App, custom: bool, change: impl FnOnce(&mut Video)) -> Vec<Effect> {
    let before = app.settings.video.clone();
    change(&mut app.settings.video);
    let changed = changed_options(&before, &app.settings.video);
    if custom && !changed.is_empty() {
        app.settings.video.preset = QualityPreset::Custom;
    }
    let mut effects = app.settings_changed();
    if app.host.room.is_none() {
        return effects;
    }
    let mut next_room = false;
    for option in changed {
        match (Video::when(option), option) {
            (When::Live, VideoOption::Codec) => {
                let video = &app.settings.video;
                tracing::info!(codec = ?video.codec, encoder = ?video.encoder, "códec para la sala abierta");
                effects.push(Effect::SetCodec);
            }
            (When::Live, VideoOption::Bitrate) => {
                let kbps = app.settings.video.max_bitrate_kbps;
                tracing::info!(kbps, "tope del bitrate para la sala abierta");
                effects.push(Effect::SetMaxBitrate(kbps));
            }
            (When::Live, other) => {
                tracing::warn!(?other, "opción en vivo sin forma de aplicarla a la sala");
            }
            (When::NextRoom, _) => next_room = true,
        }
    }
    if next_room {
        app.toast(Tone::Info, t!("host-guardado-para-la-proxima-sala"));
    }
    effects
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Mode(mode) => return set_mode(app, mode),
        Msg::Target(TargetOption(target)) => {
            app.host.target = Some(target);
            app.host.only_chosen = false;
            app.host.auto_select();
        }
        Msg::AppAudio(key, name, on) => {
            let key = normalize(&key);
            let mut effects = Vec::new();
            if app.host.mode.only_its_audio() {
                app.host.only.retain(|known| *known != key);
                if on {
                    app.host.only.push(key);
                }
                app.host.only_chosen = true;
            } else {
                // Al compartir la pantalla o una ventana, quitar una aplicación se recuerda.
                let excluded = &mut app.settings.audio.excluded_apps;
                excluded.retain(|known| normalize(&known.key) != key);
                if !on {
                    excluded.push(RememberedApp { key, name });
                }
                effects.extend(app.settings_changed());
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
        Msg::AudioOpen(open) => app.host.audio_open = open,
        // Los dos son ajustes (Ajustes → Audio y voz, y → Sala): aquí se ven y se cambian igual.
        Msg::Voice(voice) => {
            app.settings.audio.host_voice = voice;
            return app.settings_changed();
        }
        Msg::GuestsMuted(muted) => {
            app.settings.room.guests_muted = muted;
            return app.settings_changed();
        }
        Msg::Pick => {
            if app.host.picking || app.host.starting || app.host.switching.is_some() {
                return Vec::new();
            }
            app.host.picking = true;
            return vec![Effect::PickShare(app.host.source())];
        }
        Msg::Mark(user, marked) => {
            if marked {
                app.host.invite.insert(user);
            } else {
                app.host.invite.remove(&user);
            }
        }
        Msg::ReloadFriends => {
            if !app.friends.is_loading() {
                app.friends = Load::Loading;
                return vec![Effect::LoadFriends];
            }
        }
        Msg::Preset(preset) => return change_video(app, false, |video| preset.apply(video)),
        Msg::Advanced(open) => app.host.advanced = open,
        Msg::Resolution(choice) => {
            return change_video(app, true, |video| video.max_resolution = choice.value);
        }
        Msg::Fps(fps) => return change_video(app, true, |video| video.fps = fps),
        Msg::Codec(choice) => return change_video(app, true, |video| video.codec = choice.value),
        Msg::Encoder(choice) => {
            return change_video(app, true, |video| video.encoder = choice.value);
        }
        Msg::Capped(capped) => {
            let current = app.settings.video.max_bitrate_kbps;
            let kbps = if capped {
                app.host
                    .last_cap_kbps
                    .unwrap_or(Video::default().max_bitrate_kbps)
            } else {
                if current < UNLIMITED_KBPS {
                    app.host.last_cap_kbps = Some(current);
                }
                UNLIMITED_KBPS
            };
            return change_video(app, true, |video| video.max_bitrate_kbps = kbps);
        }
        Msg::Cap(choice) => {
            app.host.last_cap_kbps = Some(choice.value);
            return change_video(app, true, |video| video.max_bitrate_kbps = choice.value);
        }
        Msg::Chroma444(on) => return change_video(app, true, |video| video.chroma_444 = on),
        Msg::Hdr(on) => return change_video(app, true, |video| video.hdr = on),
        Msg::Start => return start(app),
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
        Msg::PadOn(id, on) => {
            if !on {
                return set_permissions(app, id, |p| p.gamepad_slot = None);
            }
            let max = coroom_session::input::max_gamepads();
            let free = pad_choices(app.settings.input.preferred_slot, max)
                .into_iter()
                .filter_map(|slot| slot.0)
                .find(|slot| !slot_taken(app, id, *slot));
            match free {
                Some(slot) => return set_permissions(app, id, |p| p.gamepad_slot = Some(slot)),
                None => app.toast(Tone::Warning, t!("host-sin-ranuras-libres")),
            }
        }
        Msg::Pad(id, slot) => {
            if slot.0.is_some_and(|wanted| slot_taken(app, id, wanted)) {
                app.toast(Tone::Warning, t!("host-la-slot-ya-es-de-otro", slot = slot));
                return Vec::new();
            }
            return set_permissions(app, id, |p| p.gamepad_slot = slot.0);
        }
        Msg::PadMode(id, mode) => return set_permissions(app, id, |p| p.gamepad_raw = mode.0),
        Msg::GuestVoice(id, on) => return set_permissions(app, id, |p| p.voice = on),
        Msg::Hear(id, hear) => {
            if let Some(room) = app.host.room.as_mut() {
                if hear {
                    room.muted.remove(&id);
                } else {
                    room.muted.insert(id);
                }
                return vec![Effect::SetVoiceMuted {
                    host: true,
                    source: id.0,
                    muted: !hear,
                }];
            }
        }
        Msg::Kick(id) => {
            if let Some(status) = guest(app, id) {
                app.dialog = Some(Dialog::Kick {
                    guest: id,
                    name: status.name.clone(),
                });
            }
        }
        Msg::Safety(id) => {
            if let Some(status) = guest(app, id) {
                app.dialog = Some(Dialog::SafetyNumber {
                    name: status.name.clone(),
                    fingerprint: status.fingerprint.clone(),
                    number: status.safety_number.to_string(),
                });
            }
        }
        Msg::Invite(user) => return app.update(Message::Home(super::home::Msg::Invite(user))),
        Msg::ChangeSource(open) => return change_source(app, open),
        Msg::ApplySource => return apply_source(app),
    }
    Vec::new()
}

/// Cambia lo que se comparte en el segmentado.
fn set_mode(app: &mut App, mode: ShareMode) -> Vec<Effect> {
    let host = &mut app.host;
    if host.mode == mode {
        return Vec::new();
    }
    let source = host.source();
    host.mode = mode;
    host.only_chosen = false;
    if mode.source() != source {
        host.target = None;
        host.picked = None;
        host.only.clear();
        host.auto_select();
        return refresh(app);
    }
    // Ventana ↔ Juego: la misma ventana con otro audio; con la sala abierta, se aplica ya.
    host.only = if mode.only_its_audio() {
        host.likely_game().into_iter().collect()
    } else {
        Vec::new()
    };
    if host.room.is_some() && !host.source_changed() {
        return vec![Effect::SetAppAudio(
            host.game_capture(&app.settings.audio.excluded_apps),
        )];
    }
    Vec::new()
}

/// Abre la sala con lo elegido.
fn start(app: &mut App) -> Vec<Effect> {
    if app.host.is_open() {
        return Vec::new();
    }
    let audio = app.host.game_capture(&app.settings.audio.excluded_apps);
    let guests_muted = app.settings.room.guests_muted;
    let voice = app.settings.audio.host_voice;
    let host = &mut app.host;
    host.starting = true;
    host.error = None;
    host.shared = Some(host.sharing());
    host.switching = None;
    host.changing_source = false;
    // La sala de espera sigue hasta que la sala esté abierta: así los amigos te siguen viendo
    // conectado mientras arranca (y si no arranca).
    vec![Effect::StartHost(HostRequest {
        source: host.source(),
        share: host.share_choice(),
        audio,
        voice,
        guests_muted,
    })]
}

/// Con la sala abierta, despliega el selector de fuente o lo cancela (vuelve a lo que se
/// transmite).
fn change_source(app: &mut App, open: bool) -> Vec<Effect> {
    let host = &mut app.host;
    if host.room.is_none() {
        return Vec::new();
    }
    host.changing_source = open;
    if open || host.switching.is_some() {
        return Vec::new();
    }
    let Some(shared) = host.shared.clone() else {
        return Vec::new();
    };
    let reload = shared.mode.source() != host.source();
    host.mode = shared.mode;
    host.target = shared.target;
    host.picked = shared.picked;
    if reload {
        return refresh(app);
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
        source: host.source(),
        share: host.share_choice(),
        audio: host.game_capture(&app.settings.audio.excluded_apps),
        voice: app.settings.audio.host_voice,
        guests_muted: app.settings.room.guests_muted,
    })]
}

/// Pide mirar otra vez qué se puede compartir (y la lista de amigos, si no se tiene).
pub fn refresh(app: &mut App) -> Vec<Effect> {
    if app.host.starting {
        return Vec::new();
    }
    app.host.refreshing = true;
    let mut effects = vec![Effect::LoadShareOptions(app.host.source())];
    if matches!(app.friends, Load::Idle) && app.account.is_some() {
        app.friends = Load::Loading;
        effects.push(Effect::LoadFriends);
    }
    effects
}

/// Llegó la lista de amigos o un cambio de presencia (para «en vivo · hace N s»).
pub fn on_friends(app: &mut App) {
    app.host.friends_at = Some(app.now);
}

/// Lo que se puede compartir, recién mirado.
pub fn on_share_options(app: &mut App, options: ShareOptions) {
    app.host.refreshing = false;
    app.host.refreshed_at = Some(app.now);
    if options.source != app.host.source() {
        // Llegó tarde, para la fuente de antes: se vuelve a mirar en el próximo `Tick`.
        app.host.refreshed_at = None;
        return;
    }
    // Si no se pudo listar qué compartir, se avisa una vez (la lista se mira cada pocos
    // segundos) y el selector lo explica mientras dure.
    let known = app
        .host
        .current_options()
        .and_then(|old| old.targets_error.as_ref());
    if let Some(err) = &options.targets_error
        && known != Some(err)
    {
        let text = if options.source == SourceChoice::App {
            t!("host-no-se-pudieron-listar-las-ventanas", error = err)
        } else {
            t!("host-no-se-pudieron-listar-los-monitores", error = err)
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
    if source != app.host.source() {
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
            t!("host-no-se-pudo-abrir-el-selector", error = err),
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
            let guests_muted = app.settings.room.guests_muted;
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
                muted: BTreeSet::new(),
                guests_muted,
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
            let host = &mut app.host;
            host.shared = None;
            host.switching = None;
            host.starting = false;
            host.stopping = false;
            host.changing_source = false;
            if let Some(reason) = reason {
                app.toast(
                    Tone::Error,
                    t!("host-la-sala-se-cerro-reason", reason = reason),
                );
            } else if was_open {
                app.toast(Tone::Info, t!("host-has-dejado-de-compartir"));
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
        Event::VideoChanged(result) => {
            if app.host.room.is_some() {
                match result {
                    Ok(Some(kbps)) => app.toast(
                        Tone::Info,
                        t!(
                            "host-video-de-la-sala-cambiado",
                            valor = bitrate_phrase(kbps)
                        ),
                    ),
                    Ok(None) => app.toast(Tone::Info, t!("host-codec-de-la-sala-cambiado")),
                    Err(err) => app.toast(
                        Tone::Warning,
                        t!("host-no-se-pudo-cambiar-el-video", error = err),
                    ),
                }
            }
        }
        Event::SourceSwitched(result) => {
            let requested = app.host.switching.take();
            if app.host.room.is_none() {
                return Vec::new();
            }
            match result {
                Ok(()) => {
                    app.toast(Tone::Info, t!("host-ahora-compartes-otra-cosa"));
                    if requested.is_some() {
                        app.host.shared = requested;
                    }
                    app.host.changing_source = false;
                }
                Err(err) => app.toast(
                    Tone::Warning,
                    t!("host-no-se-pudo-cambiar-la-fuente", error = err),
                ),
            }
        }
        _ => {}
    }
    Vec::new()
}

fn session_event(app: &mut App, event: SessionEvent) -> Vec<Effect> {
    let mut effects = Vec::new();
    if app.host.room.is_none() {
        return effects;
    }
    match event {
        SessionEvent::RoomCreated { code, fingerprint } => {
            if let Some(room) = app.host.room.as_mut() {
                room.code = Some(code);
                room.fingerprint = fingerprint;
            }
            effects.extend(invite_marked(app));
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
            effects.extend(app.settings_changed());
            app.toast(
                Tone::Info,
                t!(
                    "host-name-ha-entrado-en-la-sala",
                    name = name,
                    valor = permissions_label(&permissions)
                ),
            );
        }
        SessionEvent::GuestLeft { guest, kicked } => {
            let Some(room) = app.host.room.as_mut() else {
                return effects;
            };
            let name = room
                .guests
                .iter()
                .find(|status| status.guest == guest)
                .map_or_else(
                    || t!("host-el-invitado", valor = guest.0),
                    |s| s.name.clone(),
                );
            room.guests.retain(|status| status.guest != guest);
            room.muted.remove(&guest);
            app.toast(
                Tone::Neutral,
                if kicked {
                    t!("host-name-ha-sido-expulsado-las-claves", name = name)
                } else {
                    t!("host-name-ha-salido-de-la-sala", name = name)
                },
            );
        }
        SessionEvent::VideoCodec { codec, detail } => {
            if let Some(room) = app.host.room.as_mut() {
                let encoder = detail
                    .trim()
                    .strip_prefix(t!("host-codifica"))
                    .unwrap_or(detail.trim());
                room.codec = Some(t!(
                    "host-codec-por",
                    codec = codec,
                    encoder = encoder.trim()
                ));
            }
        }
        SessionEvent::RelayUnavailable(message) => {
            if let Some(room) = app.host.room.as_mut() {
                room.relay = Some(message);
            }
        }
        SessionEvent::Warning(message) => app.toast(Tone::Warning, message),
        _ => {}
    }
    effects
}

/// Con el código de la sala ya creado, invita a los amigos marcados que sigan conectados.
fn invite_marked(app: &mut App) -> Vec<Effect> {
    let marked = std::mem::take(&mut app.host.invite);
    let Some(friends) = app.friends.loaded() else {
        return Vec::new();
    };
    let invited: Vec<_> = friends
        .friends
        .iter()
        .filter(|friend| friend.online && marked.contains(&friend.id))
        .map(|friend| (friend.id, friend.display_name.clone()))
        .collect();
    if invited.is_empty() {
        return Vec::new();
    }
    let names: Vec<String> = invited.iter().map(|(_, name)| name.clone()).collect();
    app.toast(
        Tone::Info,
        t!("host-invitacion-enviada", names = join_names(&names)),
    );
    invited
        .into_iter()
        .map(|(user, _)| Effect::Invite(user))
        .collect()
}

/// «Bea», «Bea y Carla», «Bea, Carla y Dani».
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => t!("host-y-last", valor = rest.join(", "), last = last),
    }
}

// --- Textos de vídeo ------------------------------------------------------------------------------

/// El bitrate dentro de una frase: «Auto hasta 20 Mbit/s» o «Auto sin tope». Auto es el único
/// modo: el control de tasa sube hasta donde la red aguanta y baja antes de que se congele; el
/// tope solo le pone techo.
pub fn bitrate_phrase(kbps: u32) -> String {
    if kbps < UNLIMITED_KBPS {
        t!("host-auto-hasta", valor = cap_label(kbps))
    } else {
        t!("host-auto-sin-tope").to_owned()
    }
}

/// Un tope en el selector: «20 Mbit/s» (sin decimales si son Mbit/s enteros).
pub fn cap_label(kbps: u32) -> String {
    if kbps >= 1000 && kbps.is_multiple_of(1000) {
        format!("{} Mbit/s", kbps / 1000)
    } else {
        stats::kbps(f64::from(kbps))
    }
}

/// Resolución máxima, corta («1080p», «Nativa»: la de la fuente hasta 4K).
fn resolution_short(resolution: Resolution) -> String {
    match resolution {
        Resolution::P2160 => t!("host-nativa").to_owned(),
        other => format!("{}p", other.size().1),
    }
}

/// Resolución máxima en el desplegable.
fn resolution_label(resolution: Resolution) -> String {
    match resolution {
        Resolution::P2160 => t!("host-nativa-hasta-4k").to_owned(),
        other => {
            let (width, height) = other.size();
            format!("{height}p ({width} × {height})")
        }
    }
}

/// Lo que fija un perfil: «1080p · 60 fps · Auto hasta 10 Mbit/s».
fn video_spec(video: &Video) -> String {
    let mut parts = vec![
        resolution_short(video.max_resolution),
        t!("host-fps", fps = video.fps),
        bitrate_phrase(video.max_bitrate_kbps),
    ];
    if video.chroma_444 {
        parts.push("4:4:4".to_owned());
    }
    if video.hdr {
        parts.push("HDR".to_owned());
    }
    parts.join(" · ")
}

fn preset_name(preset: QualityPreset) -> &'static str {
    match preset {
        QualityPreset::MinLatency => t!("host-perfil-min-latencia"),
        QualityPreset::Balanced => t!("host-perfil-equilibrado"),
        QualityPreset::MaxQuality => t!("host-perfil-max-calidad"),
        QualityPreset::Custom => t!("host-perfil-personalizado"),
    }
}

fn preset_short(preset: QualityPreset) -> &'static str {
    match preset {
        QualityPreset::MinLatency => t!("host-perfil-min-latencia-corto"),
        QualityPreset::Balanced => t!("host-perfil-equilibrado"),
        QualityPreset::MaxQuality => t!("host-perfil-max-calidad-corto"),
        QualityPreset::Custom => t!("host-perfil-personalizado"),
    }
}

fn preset_help(preset: QualityPreset) -> &'static str {
    match preset {
        QualityPreset::MinLatency => t!("host-perfil-min-latencia-ayuda"),
        QualityPreset::Balanced => t!("host-perfil-equilibrado-ayuda"),
        QualityPreset::MaxQuality => t!("host-perfil-max-calidad-ayuda"),
        QualityPreset::Custom => t!("host-perfil-personalizado-ayuda"),
    }
}

fn preset_icon(preset: QualityPreset) -> Icon {
    match preset {
        QualityPreset::MinLatency => Icon::Zap,
        QualityPreset::Balanced => Icon::Gauge,
        QualityPreset::MaxQuality => Icon::Image,
        QualityPreset::Custom => Icon::Sliders,
    }
}

fn when_label(when: When) -> &'static str {
    match when {
        When::Live => t!("host-en-vivo"),
        When::NextRoom => t!("host-al-abrir-la-sala"),
    }
}

fn option_name(option: VideoOption) -> &'static str {
    match option {
        VideoOption::Resolution => t!("host-opcion-resolucion"),
        VideoOption::Fps => t!("host-opcion-fps"),
        VideoOption::Codec => t!("host-opcion-codec"),
        VideoOption::Bitrate => t!("host-opcion-bitrate"),
        VideoOption::Chroma444 => "4:4:4",
        VideoOption::Hdr => "HDR",
    }
}

/// Cuándo se aplica cada opción, agrupado («En vivo: códec y bitrate · Al abrir la sala: …»),
/// sacado de [`Video::when`].
fn when_summary() -> String {
    const ALL: [VideoOption; 6] = [
        VideoOption::Codec,
        VideoOption::Bitrate,
        VideoOption::Resolution,
        VideoOption::Fps,
        VideoOption::Chroma444,
        VideoOption::Hdr,
    ];
    [When::Live, When::NextRoom]
        .into_iter()
        .filter_map(|when| {
            let names: Vec<String> = ALL
                .into_iter()
                .filter(|option| Video::when(*option) == when)
                .map(|option| option_name(option).to_owned())
                .collect();
            (!names.is_empty()).then(|| {
                t!(
                    "host-cuando-grupo",
                    when = when_label(when),
                    valor = join_names(&names)
                )
            })
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn codec_choices() -> Vec<Pick<CodecChoice>> {
    vec![
        pick(CodecChoice::Auto, t!("host-automatico")),
        pick(CodecChoice::Av1, "AV1"),
        pick(CodecChoice::Hevc, "HEVC"),
        pick(CodecChoice::H264, "H.264"),
    ]
}

/// Codificadores de esta plataforma.
fn encoder_choices() -> Vec<Pick<EncoderChoice>> {
    let mut encoders = vec![pick(EncoderChoice::Auto, t!("host-automatico"))];
    encoders.extend(
        [
            (EncoderChoice::Nvenc, "NVENC (NVIDIA)"),
            (EncoderChoice::Amf, "AMF (AMD)"),
            (EncoderChoice::Qsv, "Quick Sync (Intel)"),
            (EncoderChoice::Vulkan, "Vulkan Video"),
            (EncoderChoice::Vaapi, "VA-API"),
        ]
        .into_iter()
        .filter(|(c, _)| cfg!(target_os = "linux") || *c != EncoderChoice::Vaapi)
        .map(|(c, label)| pick(c, label)),
    );
    encoders.push(pick(EncoderChoice::Software, t!("host-por-software")));
    encoders
}

/// Topes que se ofrecen con «Tope» marcado (sin el último, que es «sin tope»).
fn cap_choices() -> Vec<Pick<u32>> {
    BITRATE_CHOICES
        .iter()
        .filter(|kbps| **kbps < UNLIMITED_KBPS)
        .map(|kbps| pick(*kbps, cap_label(*kbps)))
        .collect()
}

/// Códec elegido (`None`: automático).
fn chosen_codec(app: &App) -> Option<coroom_session::codec::Codec> {
    use coroom_session::codec::Codec;
    match app.settings.video.codec {
        CodecChoice::Auto => None,
        CodecChoice::Av1 => Some(Codec::Av1),
        CodecChoice::Hevc => Some(Codec::Hevc),
        CodecChoice::H264 => Some(Codec::H264),
    }
}

fn join_codecs(names: impl Iterator<Item = String>) -> String {
    let names: Vec<String> = names.collect();
    join_names(&names)
}

/// Explicación de 4:4:4 con lo que hace este equipo.
pub fn chroma_hint(app: &App) -> String {
    let lead = t!("host-croma-lead");
    let Some(support) = &app.video_support else {
        return t!("host-croma-comprobando", lead = lead);
    };
    if support.yuv444.is_empty() {
        let decode = if support.decode_yuv444.is_empty() {
            String::new()
        } else {
            t!(
                "host-croma-como-invitado",
                valor = join_codecs(support.decode_yuv444.iter().map(ToString::to_string))
            )
        };
        return t!("host-croma-no-disponible", lead = lead, decode = decode);
    }
    let with = join_codecs(support.yuv444.iter().map(|(_, label)| label.clone()));
    match chosen_codec(app) {
        Some(codec) if !support.yuv444.iter().any(|(c, _)| *c == codec) => t!(
            "host-croma-no-con-este-codec",
            lead = lead,
            codec = codec,
            valor = with
        ),
        _ => t!("host-croma-disponible-con", lead = lead, valor = with),
    }
}

/// Explicación de HDR con lo que hace este equipo.
pub fn hdr_hint(app: &App) -> String {
    let Some(support) = &app.video_support else {
        return t!("host-hdr-comprobando").to_owned();
    };
    let share = if support.hdr10.is_empty() {
        t!("host-hdr-no-disponible").to_owned()
    } else {
        let with = join_codecs(support.hdr10.iter().map(|(_, label)| label.clone()));
        match chosen_codec(app) {
            Some(codec) if !support.hdr10.iter().any(|(c, _)| *c == codec) => {
                t!("host-hdr-no-con-este-codec", codec = codec, valor = with)
            }
            _ => t!("host-hdr-disponible-con", valor = with),
        }
    };
    let display = match support.hdr_display {
        Some(true) => t!("host-hdr-pantalla-si"),
        Some(false) => t!("host-hdr-pantalla-no"),
        None => t!("host-hdr-pantalla-quiza"),
    };
    format!("{share} {display}")
}

// --- Vista ----------------------------------------------------------------------------------------

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

/// Etiqueta «En vivo» (verde) o «Al abrir la sala» (gris) de una opción.
fn tag<'a>(when: When) -> Element<'a, Message> {
    match when {
        When::Live => coroom_ui::tag::live(when_label(when)),
        When::NextRoom => coroom_ui::tag::later(when_label(when)),
    }
}

/// Desplegable que ocupa el ancho que le dejen.
fn select_fill<'a, T>(
    options: Vec<T>,
    selected: Option<T>,
    on_select: impl Fn(T) -> Message + 'a,
) -> Element<'a, Message>
where
    T: ToString + PartialEq + Clone + 'a,
{
    iced_widget::pick_list(options, selected, on_select)
        .padding(Padding::from([8.0, 12.0]))
        .text_size(type_size::BODY)
        .font(coroom_ui::font::UI)
        .width(Length::Fill)
        .into()
}

/// Recuadro 16:9 del vídeo con `content` dentro.
fn thumb<'a>(content: Element<'a, Message>, width: f32) -> Element<'a, Message> {
    container(content)
        .width(Length::Fixed(width))
        .height(Length::Fixed((width * 9.0 / 16.0).round()))
        .class(ContainerClass::Video)
        .into()
}

/// Icono y texto centrados sobre el fondo del vídeo (sin vista previa).
fn placeholder<'a>(glyph: Icon, label: &'a str) -> Element<'a, Message> {
    container(
        column![
            icon_with(glyph, 24.0, SvgClass::Overlay),
            iced_widget::text(label)
                .size(type_size::CAPTION)
                .class(TextClass::Overlay)
                .align_x(iced_core::alignment::Horizontal::Center)
                .wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(space::XS)
        .align_x(Alignment::Center),
    )
    .padding(space::XS)
    .center(Length::Fill)
    .into()
}

/// Botón de solo icono que se enciende con el acento (un permiso dado).
fn toggle<'a>(glyph: Icon, active: bool, tip: &'a str, message: Message) -> Element<'a, Message> {
    let (class, tint) = if active {
        (ButtonClass::Primary, SvgClass::OnAccent)
    } else {
        (ButtonClass::Flat, SvgClass::Secondary)
    };
    ui::with_tooltip(
        button(
            container(icon_with(glyph, size::ICON, tint))
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        )
        .width(Length::Fixed(size::CONTROL))
        .height(Length::Fixed(size::CONTROL))
        .padding(0)
        .class(class)
        .on_press(message),
        tip,
    )
}

/// Cómo se nombra lo elegido en el selector del sistema (Wayland solo da el tamaño).
fn picked_label(target: &CaptureTarget) -> String {
    match target {
        CaptureTarget::Monitor { name, .. } if name.is_empty() => t!("host-monitor").to_owned(),
        CaptureTarget::Monitor { name, .. } => t!("host-monitor-de-size", size = name.as_str()),
        CaptureTarget::Window { title, .. } => t!("host-ventana-de-size", size = title.as_str()),
    }
}

/// Cómo se nombra una transmisión («Monitor principal · 2560 × 1440», «Fatal Fury»…).
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
    match (sharing.mode, target) {
        (ShareMode::Test, _) => t!("host-la-fuente-de-prueba").to_owned(),
        (_, Some(target)) => target,
        (ShareMode::Screen, None) => t!("host-modo-pantalla").to_owned(),
        (_, None) => t!("host-modo-ventana").to_owned(),
    }
}

/// El monitor o la ventana: la lista de aquí, lo elegido en el selector del sistema con su botón,
/// o por qué no se puede.
fn target_control(state: &State) -> Element<'_, Message> {
    let window = state.source() == SourceChoice::App;
    let options = state.current_options();
    if state.mode == ShareMode::Test {
        return column![
            ui::body(t!("host-fuente-de-prueba-ayuda")).wrapping(Wrapping::WordOrGlyph),
            ui::button_with(
                Kind::Link,
                t!("host-volver-a-la-pantalla"),
                None,
                Some(on(Msg::Mode(ShareMode::Screen))),
            ),
        ]
        .spacing(space::XXS)
        .into();
    }
    if let Some(err) = options.and_then(|options| options.targets_error.as_ref())
        && options.is_some_and(|options| options.targets.is_empty())
    {
        return column![
            ui::strong(if window {
                t!("host-no-se-pudieron-listar-las-ventanas-titulo")
            } else {
                t!("host-no-se-pudieron-listar-los-monitores-titulo")
            }),
            ui::caption(err.as_str()).wrapping(Wrapping::WordOrGlyph),
        ]
        .spacing(2)
        .into();
    }
    let Some(options) = options else {
        // Aún no se ha mirado, o lo que hay es de la otra fuente.
        return ui::secondary(t!("host-buscando")).into();
    };
    if state.system_picks() {
        return picked_control(state);
    }
    let targets: Vec<TargetOption> = options.targets.iter().cloned().map(TargetOption).collect();
    select_fill(targets, state.target.clone().map(TargetOption), |target| {
        on(Msg::Target(target))
    })
}

/// Lo elegido en el selector del sistema, con el botón que lo abre.
fn picked_control(state: &State) -> Element<'_, Message> {
    let window = state.source() == SourceChoice::App;
    let busy = state.picking || state.switching.is_some();
    let (text, subtitle) = match (&state.picked, state.picking) {
        (_, true) => (t!("host-elige-en-el-selector-del-sistema").to_owned(), None),
        (Some(picked), false) => (picked_label(&picked.target), None),
        (None, false) => (
            if window {
                t!("host-aun-no-has-elegido-ventana")
            } else {
                t!("host-aun-no-has-elegido-monitor")
            }
            .to_owned(),
            (!state.is_open()).then(|| t!("host-si-no-eliges-ahora")),
        ),
    };
    let action = match (&state.picked, window) {
        (None, _) => t!("host-elegir"),
        (Some(_), false) => t!("host-cambiar-puntos"),
        (Some(_), true) => t!("host-elegir-otra"),
    };
    let mut texts = column![ui::body(text).wrapping(Wrapping::WordOrGlyph)].spacing(2);
    if let Some(subtitle) = subtitle {
        texts = texts.push(ui::caption(subtitle).wrapping(Wrapping::WordOrGlyph));
    }
    // En columna: al lado de la miniatura no cabe el texto con el botón a la derecha.
    column![
        texts,
        ui::button_with(Kind::Normal, action, None, (!busy).then(|| on(Msg::Pick))),
    ]
    .spacing(space::XS)
    .into()
}

/// Segmentado Pantalla / Ventana / Juego.
fn mode_picker(state: &State) -> Element<'_, Message> {
    ui::segmented(
        ShareMode::PICKER
            .iter()
            .map(|mode| {
                (
                    mode.label().to_owned(),
                    on(Msg::Mode(*mode)),
                    state.mode == *mode,
                )
            })
            .collect(),
    )
}

/// «Qué compartir» antes de abrir la sala (`real`: la de verdad o la copia que da la altura).
fn source_card(app: &App, real: bool) -> Element<'_, Message> {
    let state = &app.host;
    let head = row![
        ui::strong(t!("host-que-compartir")),
        ui::fill(),
        mode_picker(state)
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    // La vista previa en vivo no existe antes de abrir la sala: la captura arranca con ella.
    let preview = thumb(
        placeholder(state.mode.icon(), t!("host-vista-previa-al-abrir")),
        THUMB_WIDTH,
    );
    let mut side = column![target_control(state)].spacing(space::XS);
    if state.mode != ShareMode::Test {
        let mut help = column![ui::caption(state.mode.help()).wrapping(Wrapping::WordOrGlyph)];
        if state.source() == SourceChoice::App {
            help =
                help.push(ui::caption(t!("host-ventana-en-negro")).wrapping(Wrapping::WordOrGlyph));
        }
        help = help
            .push(ui::caption(t!("host-se-puede-cambiar-dentro")).wrapping(Wrapping::WordOrGlyph));
        side = side.push(help.spacing(2));
    }
    let body = row![preview, side.width(Length::Fill)]
        .spacing(space::S)
        .align_y(Alignment::Center);
    let card = column![head, body].spacing(space::S);
    container(card)
        .padding(Padding::from([space::S, space::M]))
        .width(Length::Fill)
        .height(if real { Length::Fill } else { Length::Shrink })
        .class(ContainerClass::Card)
        .into()
}

/// «en vivo · hace N s»: desde la última lista de amigos o el último cambio de presencia.
fn live_label(app: &App) -> Option<String> {
    let at = app.host.friends_at?;
    let secs = app.now.saturating_duration_since(at).as_secs();
    Some(if secs < 60 {
        t!("host-en-vivo-hace-segundos", n = secs)
    } else {
        t!("host-en-vivo-hace-minutos", n = secs / 60)
    })
}

/// `list` con una línea y `element` debajo.
fn with_separator<'a>(
    list: Column<'a, Message, coroom_ui::Theme>,
    element: Element<'a, Message>,
) -> Column<'a, Message, coroom_ui::Theme> {
    list.push(rule::horizontal(1)).push(element)
}

/// «A quién invitar»: los amigos con su casilla (los conectados arriba; los desconectados, sin
/// casilla).
fn invite_card(app: &App, real: bool) -> Element<'_, Message> {
    let state = &app.host;
    let mut header = row![ui::strong(t!("host-a-quien-invitar")), ui::fill()]
        .spacing(space::S)
        .align_y(Alignment::Center);
    if let Some(label) = live_label(app)
        && app.friends.loaded().is_some()
    {
        header = header.push(ui::caption(label));
    }
    let mut list = Column::new().push(container(header).padding(Padding::from([12.0, 16.0])));
    let push = with_separator;
    match &app.friends {
        Load::Loaded(friends) if friends.friends.is_empty() => {
            list = push(
                list,
                container(ui::secondary(t!("host-sin-amigos")).wrapping(Wrapping::WordOrGlyph))
                    .padding(Padding::from([10.0, 16.0]))
                    .into(),
            );
        }
        Load::Loaded(friends) => {
            let mut sorted: Vec<_> = friends.friends.iter().collect();
            sorted.sort_by_key(|friend| !friend.online);
            for friend in sorted {
                let id = friend.id;
                let mark: Element<'_, Message> = if friend.online {
                    checkbox(state.invite.contains(&id))
                        .on_toggle(move |marked| on(Msg::Mark(id, marked)))
                        .size(18.0)
                        .into()
                } else {
                    ui::gap(18.0)
                };
                let name: Element<'_, Message> = if friend.online {
                    ui::strong(friend.display_name.as_str()).into()
                } else {
                    ui::secondary(friend.display_name.as_str()).into()
                };
                let badge = if friend.online {
                    ui::status_badge(Tone::Success, t!("host-disponible"))
                } else {
                    ui::status_badge(Tone::Neutral, t!("host-desconectado"))
                };
                list = push(
                    list,
                    container(
                        row![
                            mark,
                            ui::avatar(&app.theme, &friend.display_name, 28.0),
                            container(name).width(Length::Fill),
                            badge
                        ]
                        .spacing(space::S)
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::from([6.0, 16.0]))
                    .into(),
                );
            }
        }
        Load::Failed(err) => {
            list = push(
                list,
                container(ui::banner(
                    Tone::Error,
                    err.as_str(),
                    Some((t!("host-reintentar"), on(Msg::ReloadFriends))),
                ))
                .padding(space::XS)
                .into(),
            );
        }
        Load::Idle | Load::Loading => {
            list = push(
                list,
                container(ui::skeleton(t!("host-cargando-tus-amigos"), 2))
                    .padding(Padding::from([10.0, 16.0]))
                    .into(),
            );
        }
    }
    container(list)
        .width(Length::Fill)
        .height(if real { Length::Fill } else { Length::Shrink })
        .class(ContainerClass::Card)
        .into()
}

/// Tarjeta grande de un perfil de calidad (`real`: la de verdad o la copia que da la altura).
fn preset_card(video: &Video, preset: QualityPreset, real: bool) -> Element<'_, Message> {
    let selected = video.preset == preset;
    let mut values = video.clone();
    preset.apply(&mut values);
    let lead = if selected {
        icon_with(Icon::CircleCheck, 18.0, SvgClass::Accent)
    } else {
        icon_with(preset_icon(preset), 18.0, SvgClass::Accent)
    };
    let content = column![
        row![lead, ui::strong(preset_name(preset)).size(15.0)]
            .spacing(space::XS)
            .align_y(Alignment::Center),
        ui::secondary(preset_help(preset)).wrapping(Wrapping::WordOrGlyph),
        ui::caption(video_spec(&values)).wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(6)
    .width(Length::Fill);
    // «Personalizado» abre Avanzado, donde está lo que lo define.
    let message = if preset == QualityPreset::Custom {
        on(Msg::Advanced(true))
    } else {
        on(Msg::Preset(preset))
    };
    button(content)
        .padding(14)
        .width(Length::Fill)
        .height(if real { Length::Fill } else { Length::Shrink })
        .class(ButtonClass::Choice { selected })
        .on_press(message)
        .into()
}

/// Los tres perfiles (y «Personalizado» si es el elegido), igual de altos.
fn preset_cards(app: &App) -> Element<'_, Message> {
    let video = &app.settings.video;
    let mut presets = QualityPreset::PRESETS.to_vec();
    if video.preset == QualityPreset::Custom {
        presets.push(QualityPreset::Custom);
    }
    ui::equal_height_row(
        |real| {
            presets
                .iter()
                .map(|preset| preset_card(video, *preset, real))
                .collect()
        },
        space::S,
    )
}

/// Segmentado compacto de los perfiles (con la sala abierta).
fn preset_segmented(app: &App) -> Element<'_, Message> {
    let video = &app.settings.video;
    let mut options: Vec<(String, Message, bool)> = QualityPreset::PRESETS
        .iter()
        .map(|preset| {
            (
                preset_short(*preset).to_owned(),
                on(Msg::Preset(*preset)),
                video.preset == *preset,
            )
        })
        .collect();
    if video.preset == QualityPreset::Custom {
        options.push((
            preset_short(QualityPreset::Custom).to_owned(),
            on(Msg::Advanced(true)),
            true,
        ));
    }
    ui::segmented(options)
}

/// Fila de «Avanzado»: título, explicación, la etiqueta de cuándo se aplica y el control.
fn option_row<'a>(
    title: &'a str,
    subtitle: Option<String>,
    option: VideoOption,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    ui::pref_row(
        title,
        subtitle,
        row![tag(Video::when(option)), control]
            .spacing(space::S)
            .align_y(Alignment::Center),
    )
}

/// «Avanzado», plegado o desplegado.
fn advanced(app: &App) -> Element<'_, Message> {
    let state = &app.host;
    let video = &app.settings.video;
    let open = state.advanced;
    let status = if video.preset == QualityPreset::Custom {
        preset_name(QualityPreset::Custom).to_owned()
    } else {
        t!(
            "host-avanzado-sin-cambios",
            perfil = preset_name(video.preset)
        )
    };
    let header = button(
        row![
            icon_with(
                if open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                size::ICON,
                SvgClass::Secondary
            ),
            column![
                ui::body(t!("host-avanzado")),
                ui::secondary(if open {
                    t!("host-avanzado-desplegado")
                } else {
                    t!("host-avanzado-plegado")
                })
                .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(2)
            .width(Length::Fill),
            ui::caption(status),
        ]
        .spacing(space::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([10.0, 16.0]))
    .width(Length::Fill)
    .class(ButtonClass::Flat)
    .on_press(on(Msg::Advanced(!open)));
    let mut rows: Vec<Element<'_, Message>> = vec![header.into()];
    if !open {
        return ui::boxed_list(rows);
    }
    let resolutions: Vec<Pick<Resolution>> = Resolution::ALL
        .iter()
        .rev()
        .map(|r| pick(*r, resolution_label(*r)))
        .collect();
    let resolution = resolutions
        .iter()
        .find(|c| c.value == video.max_resolution)
        .cloned();
    rows.push(option_row(
        t!("host-resolucion-maxima"),
        Some(t!("host-resolucion-ayuda").to_owned()),
        VideoOption::Resolution,
        ui::select(resolutions, resolution, |c| on(Msg::Resolution(c))),
    ));
    let fps = ui::segmented(
        FPS_CHOICES
            .iter()
            .map(|fps| (fps.to_string(), on(Msg::Fps(*fps)), video.fps == *fps))
            .collect(),
    );
    rows.push(option_row(
        t!("host-fotogramas-por-segundo"),
        None,
        VideoOption::Fps,
        fps,
    ));
    let codecs = codec_choices();
    let codec = codecs.iter().find(|c| c.value == video.codec).cloned();
    let codec_help = match state.room.as_ref().and_then(|room| room.codec.as_ref()) {
        Some(codec) => t!("host-codec-en-uso", codec = codec.as_str()),
        None => t!("host-codec-ayuda").to_owned(),
    };
    rows.push(option_row(
        t!("host-codec"),
        Some(codec_help),
        VideoOption::Codec,
        ui::select(codecs, codec, |c| on(Msg::Codec(c))),
    ));
    let encoders = encoder_choices();
    let encoder = encoders.iter().find(|c| c.value == video.encoder).cloned();
    rows.push(option_row(
        t!("host-codificador"),
        Some(t!("host-codificador-ayuda").to_owned()),
        VideoOption::Codec,
        ui::select(encoders, encoder, |c| on(Msg::Encoder(c))),
    ));
    rows.push(bitrate_row(app));
    rows.push(option_row(
        t!("host-croma"),
        Some(chroma_hint(app)),
        VideoOption::Chroma444,
        ui::switch(video.chroma_444, Some(|on_off| on(Msg::Chroma444(on_off)))),
    ));
    rows.push(option_row(
        "HDR",
        Some(hdr_hint(app)),
        VideoOption::Hdr,
        ui::switch(video.hdr, Some(|on_off| on(Msg::Hdr(on_off)))),
    ));
    if state.room.is_none() {
        // La fuente de prueba (sin enseñar nada tuyo), para probar la conexión.
        let test = state.mode == ShareMode::Test;
        rows.push(ui::pref_row(
            t!("host-usar-la-fuente-de-prueba"),
            Some(t!("host-fuente-de-prueba-fila").to_owned()),
            ui::switch(
                test,
                Some(|on_off| {
                    on(Msg::Mode(if on_off {
                        ShareMode::Test
                    } else {
                        ShareMode::Screen
                    }))
                }),
            ),
        ));
    }
    ui::boxed_list(rows)
}

/// Bitrate: siempre Auto; con «Tope», hasta dónde puede subir.
fn bitrate_row(app: &App) -> Element<'_, Message> {
    let video = &app.settings.video;
    let capped = video.max_bitrate_kbps < UNLIMITED_KBPS;
    let mut cap = row![
        checkbox(capped)
            .on_toggle(|on_off| on(Msg::Capped(on_off)))
            .size(18.0),
        ui::body(t!("host-tope")),
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    if capped {
        let caps = cap_choices();
        let current = caps
            .iter()
            .find(|c| c.value == video.max_bitrate_kbps)
            .cloned()
            .unwrap_or_else(|| pick(video.max_bitrate_kbps, cap_label(video.max_bitrate_kbps)));
        cap = cap.push(ui::select(caps, Some(current), |c| on(Msg::Cap(c))));
    }
    // Lo que pide ahora el control de tasa del invitado más lento.
    let now = app
        .host
        .room
        .as_ref()
        .and_then(|room| room.guests.iter().filter_map(|g| g.target_kbps).min());
    let hint = match (capped, now) {
        (_, Some(kbps)) => t!("host-bitrate-ahora", valor = stats::kbps(f64::from(kbps))),
        (false, None) => t!(
            "host-sin-tope-ayuda",
            valor = cap_label(start_kbps(video.max_bitrate_kbps))
        ),
        (true, None) => t!(
            "host-con-tope-ayuda",
            valor = cap_label(start_kbps(video.max_bitrate_kbps))
        ),
    };
    cap = cap.push(ui::caption(hint).wrapping(Wrapping::WordOrGlyph));
    let texts = column![
        ui::body(t!("host-bitrate")),
        ui::secondary(t!("host-bitrate-ayuda")).wrapping(Wrapping::WordOrGlyph),
        container(cap).padding(Padding {
            top: 6.0,
            ..Padding::ZERO
        }),
    ]
    .spacing(2)
    .width(Length::Fill);
    row![
        texts,
        tag(Video::when(VideoOption::Bitrate)),
        ui::strong(t!("host-auto"))
    ]
    .spacing(space::S)
    .align_y(Alignment::Start)
    .padding(Padding::from([10.0, 16.0]))
    .into()
}

/// Fila de la lista de audio: la casilla, el nombre y la nota a la derecha.
fn audio_row<'a>(row_data: AudioRow) -> Element<'a, Message> {
    let AudioRow {
        key,
        name,
        checked,
        note,
    } = row_data;
    let toggle = on(Msg::AppAudio(key, name.clone(), !checked));
    let label: Element<'a, Message> = if checked {
        ui::body(name).into()
    } else {
        ui::secondary(name).into()
    };
    row![
        checkbox(checked)
            .on_toggle(move |_| toggle.clone())
            .size(18.0),
        container(label).width(Length::Fill),
        ui::caption(note)
    ]
    .spacing(space::S)
    .align_y(Alignment::Center)
    .padding(Padding {
        top: 8.0,
        right: 16.0,
        bottom: 8.0,
        left: 44.0,
    })
    .into()
}

/// Resumen de lo que suena en la sala: «Juego y Firefox», o por qué va todo.
fn audio_summary(app: &App) -> String {
    let state = &app.host;
    let Some(options) = state.current_options() else {
        return t!("host-buscando").to_owned();
    };
    if let Err(why) = &options.apps {
        return t!("host-este-equipo-no-separa-el-audio", why = why.as_str());
    }
    let names: Vec<String> = state
        .audio_rows(&app.settings.audio.excluded_apps)
        .into_iter()
        .filter(|row| row.checked)
        .map(|row| row.name)
        .collect();
    if names.is_empty() {
        if state.mode.only_its_audio() {
            t!("host-audio-nada").to_owned()
        } else {
            t!("host-ninguna-aplicacion-suena-ahora").to_owned()
        }
    } else {
        join_names(&names)
    }
}

/// Audio que se envía (desplegable) y, antes de abrir, la voz.
fn audio_box(app: &App, with_voice: bool) -> Element<'_, Message> {
    let state = &app.host;
    let open = state.audio_open;
    let header = button(
        row![
            icon_with(Icon::Volume, size::ICON, SvgClass::Secondary),
            column![
                ui::body(t!("host-audio-que-se-envia")),
                ui::secondary(audio_summary(app)).wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(2)
            .width(Length::Fill),
            icon_with(
                if open {
                    Icon::ChevronUp
                } else {
                    Icon::ChevronDown
                },
                size::ICON,
                SvgClass::Secondary
            ),
        ]
        .spacing(space::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([10.0, 16.0]))
    .width(Length::Fill)
    .class(ButtonClass::Flat)
    .on_press(on(Msg::AudioOpen(!open)));
    let mut rows: Vec<Element<'_, Message>> = vec![header.into()];
    if open {
        let supported = state
            .current_options()
            .is_some_and(|options| options.apps.is_ok());
        if supported {
            let subtitle = if state.mode.only_its_audio() {
                t!("host-marcada-la-aplicacion-que-elegiste")
            } else {
                t!("host-las-aplicaciones-que-empiecen-a-sonar")
            };
            let mut list = Column::new().push(
                container(ui::caption(subtitle).wrapping(Wrapping::WordOrGlyph)).padding(Padding {
                    top: 8.0,
                    right: 16.0,
                    bottom: 0.0,
                    left: 44.0,
                }),
            );
            for audio in state.audio_rows(&app.settings.audio.excluded_apps) {
                list = list.push(audio_row(audio));
            }
            list = list.push(
                container(ui::caption(t!("host-coroom-nunca-se-envia-a-si"))).padding(Padding {
                    top: 0.0,
                    right: 16.0,
                    bottom: 10.0,
                    left: 44.0,
                }),
            );
            rows.push(list.into());
        }
    }
    if with_voice {
        rows.push(ui::pref_row(
            t!("host-hablar-por-la-sala-con-el"),
            Some(t!("host-los-invitados-te-oyen-y-hablan").to_owned()),
            ui::switch(
                app.settings.audio.host_voice,
                Some(|on_off| on(Msg::Voice(on_off))),
            ),
        ));
        rows.push(ui::pref_row(
            t!("host-los-invitados-entran-callados"),
            Some(t!("host-entran-callados-ayuda").to_owned()),
            ui::switch(
                app.settings.room.guests_muted,
                Some(|on_off| on(Msg::GuestsMuted(on_off))),
            ),
        ));
    }
    ui::boxed_list(rows)
}

/// Amigos marcados que se invitarán (los que siguen conectados).
fn marked_count(app: &App) -> usize {
    app.friends.loaded().map_or(0, |friends| {
        friends
            .friends
            .iter()
            .filter(|friend| friend.online && app.host.invite.contains(&friend.id))
            .count()
    })
}

/// Antes de abrir la sala.
fn setup(app: &App) -> Element<'_, Message> {
    let state = &app.host;
    let top = ui::equal_height_row(
        |real| {
            vec![
                container(source_card(app, real))
                    .width(Length::FillPortion(23))
                    .height(if real { Length::Fill } else { Length::Shrink })
                    .into(),
                container(invite_card(app, real))
                    .width(Length::FillPortion(20))
                    .height(if real { Length::Fill } else { Length::Shrink })
                    .into(),
            ]
        },
        space::M,
    );
    let quality_head = row![
        ui::strong(t!("host-calidad")).size(15.0),
        ui::caption(t!("host-calidad-ayuda"))
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    let mut page = column![
        top,
        column![quality_head, preset_cards(app)].spacing(space::XS),
        advanced(app),
        audio_box(app, true),
    ]
    .spacing(space::M);
    if let Some(error) = &state.error {
        page = page.push(ui::banner(
            Tone::Error,
            t!("host-no-se-pudo-abrir-la-sala", error = error),
            Some((t!("host-reintentar"), on(Msg::Start))),
        ));
    }
    let actions: Element<'_, Message> = if state.starting {
        container(ui::progress(
            t!("host-abriendo-la-sala"),
            None,
            app.now.elapsed().as_secs_f32(),
        ))
        .width(Length::Fixed(420.0))
        .into()
    } else {
        let marked = marked_count(app);
        let note = match marked {
            0 => t!("host-el-codigo-aparece").to_owned(),
            1 => t!("host-invitaras-a-uno").to_owned(),
            n => t!("host-invitaras-a-n", n = n),
        };
        row![
            ui::caption(note),
            ui::button_with(
                Kind::Primary,
                if marked > 0 {
                    t!("host-abrir-la-sala-e-invitar")
                } else {
                    t!("host-abrir-la-sala")
                },
                Some(Icon::ShareScreen),
                (!state.picking).then(|| on(Msg::Start)),
            ),
        ]
        .spacing(space::S)
        .align_y(Alignment::Center)
        .into()
    };
    page = page.push(row![ui::fill(), actions].align_y(Alignment::Center));
    ui::page(page)
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

/// «directa», «por relay» o «directa y relay», según cómo llegan los invitados.
fn route_summary(room: &Room) -> Option<&'static str> {
    use coroom_transport::Path;
    let direct = room.guests.iter().any(|g| g.path == Some(Path::Direct));
    let relayed = room.guests.iter().any(|g| g.path == Some(Path::Relayed));
    match (direct, relayed) {
        (true, true) => Some(t!("host-ruta-mixta")),
        (true, false) => Some(t!("host-ruta-directa")),
        (false, true) => Some(t!("host-ruta-relay")),
        (false, false) => None,
    }
}

/// Cabecera de la sala: «Sala de Ana», el código, los invitados y la ruta.
fn room_header<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let title = app.account.as_ref().map_or_else(
        || t!("host-tu-sala").to_owned(),
        |account| t!("host-sala-de", name = account.display_name.as_str()),
    );
    let mut facts = vec![match &room.code {
        Some(code) => t!("host-codigo", code = spaced(code)),
        None => t!("host-creando-la-sala").to_owned(),
    }];
    facts.push(match room.guests.len() {
        0 => t!("host-sin-invitados").to_owned(),
        1 => t!("host-un-invitado").to_owned(),
        n => t!("host-n-invitados", n = n),
    });
    if let Some(route) = route_summary(room) {
        facts.push(route.to_owned());
    }
    let badge = match (&room.codec, room.guests.is_empty()) {
        (Some(_), _) => ui::status_badge(Tone::Success, t!("host-transmitiendo")),
        (None, true) => ui::status_badge(Tone::Neutral, t!("host-esperando-invitados")),
        (None, false) => ui::status_badge(Tone::Warning, t!("host-eligiendo-codec")),
    };
    ui::page_header(
        title,
        Some(facts.join(" · ")),
        vec![
            badge,
            ui::icon_button(
                Icon::Copy,
                t!("host-copiar-codigo"),
                room.code.as_ref().map(|_| on(Msg::CopyCode)),
            ),
            ui::button_with(
                Kind::Destructive,
                t!("host-cerrar-la-sala"),
                Some(Icon::Close),
                (!app.host.stopping).then(|| on(Msg::Stop)),
            )
            .into(),
        ],
    )
}

/// La fuente con la sala abierta: vista previa, qué es y con qué se codifica, y «Cambiar fuente».
fn room_source_card<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let state = &app.host;
    let preview: Element<'a, Message> = if room.preview_unavailable {
        placeholder(Icon::Monitor, t!("host-sin-vista-previa"))
    } else {
        video_view(&room.preview)
    };
    let title = match (&state.switching, &state.shared) {
        (Some(switching), _) => t!("host-cambiando-a", que = sharing_label(switching)),
        (None, Some(shared)) => sharing_label(shared),
        (None, None) => String::new(),
    };
    let mut facts = Vec::new();
    match &room.codec {
        Some(codec) => facts.push(codec.clone()),
        None if room.guests.is_empty() => {
            facts.push(t!("host-esperando-al-primer-invitado").to_owned());
        }
        None => facts.push(t!("host-eligiendo-el-codec-con-tus-invitados").to_owned()),
    }
    if let Some(kbps) = room.guests.iter().filter_map(|g| g.target_kbps).min() {
        facts.push(stats::kbps(f64::from(kbps)));
    }
    let info = column![
        ui::strong(title).wrapping(Wrapping::WordOrGlyph),
        ui::secondary(facts.join(" · ")).wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(2)
    .width(Length::Fill);
    let change = ui::button_with(
        Kind::Normal,
        t!("host-cambiar-fuente"),
        Some(Icon::ArrowUpDown),
        (!state.changing_source && state.switching.is_none()).then(|| on(Msg::ChangeSource(true))),
    );
    let mut card = column![
        row![thumb(preview, PREVIEW_WIDTH), info, change]
            .spacing(space::M)
            .align_y(Alignment::Center)
    ]
    .spacing(space::S);
    if state.changing_source {
        card = card.push(rule::horizontal(1)).push(source_picker(app));
    }
    ui::card(card)
}

/// El selector de fuente con la sala abierta: lo mismo que antes de abrirla, más «Compartir
/// esto».
fn source_picker(app: &App) -> Element<'_, Message> {
    let state = &app.host;
    let busy = state.picking || state.switching.is_some();
    let head = row![
        ui::strong(t!("host-que-compartir")),
        ui::fill(),
        mode_picker(state)
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    let mut help = column![ui::caption(state.mode.help()).wrapping(Wrapping::WordOrGlyph)];
    if state.source() == SourceChoice::App {
        help = help.push(ui::caption(t!("host-ventana-en-negro")).wrapping(Wrapping::WordOrGlyph));
    }
    help = help
        .push(ui::caption(t!("host-al-cambiar-la-ultima-imagen")).wrapping(Wrapping::WordOrGlyph));
    let actions = row![
        ui::fill(),
        ui::button_with(
            Kind::Normal,
            t!("app-cancelar"),
            None,
            state
                .switching
                .is_none()
                .then(|| on(Msg::ChangeSource(false))),
        ),
        ui::button_with(
            Kind::Primary,
            t!("host-compartir-esto"),
            None,
            (!busy && state.source_changed()).then(|| on(Msg::ApplySource)),
        ),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    column![head, target_control(state), help.spacing(2), actions]
        .spacing(space::S)
        .into()
}

/// Si el anfitrión silenció en su equipo la voz del invitado `id`.
fn hears_muted(room: &Room, id: GuestId) -> bool {
    room.muted.contains(&id)
}

/// Motivo corto del relay de un invitado, si se sabe: «CGNAT» cuando ninguna dirección directa
/// respondió (lo que pasa detrás de la NAT de una operadora).
fn relay_reason(status: &GuestStatus) -> Option<&'static str> {
    let silent = !status.candidates.is_empty()
        && status
            .candidates
            .iter()
            .all(|candidate| matches!(candidate.outcome, CandidateOutcome::NoResponse));
    silent.then(|| t!("host-cgnat"))
}

fn quality_label(level: QualityLevel) -> &'static str {
    match level {
        QualityLevel::Excellent => t!("host-calidad-excelente"),
        QualityLevel::Good => t!("host-calidad-buena"),
        QualityLevel::Acceptable => t!("host-calidad-aceptable"),
        QualityLevel::Poor => t!("host-calidad-mala"),
    }
}

/// Fila de un invitado: quién, cómo llega y qué puede hacer.
fn guest_row<'a>(app: &'a App, room: &'a Room, status: &'a GuestStatus) -> Element<'a, Message> {
    use coroom_transport::Path;
    let id = status.guest;
    let permissions = status.permissions;
    let mut facts = Vec::new();
    match status.path {
        Some(Path::Direct) => facts.push(t!("host-directa").to_owned()),
        Some(Path::Relayed) => {
            facts.push(t!("host-relay").to_owned());
            if let Some(reason) = relay_reason(status) {
                facts.push(reason.to_owned());
            }
        }
        None => facts.push(t!("host-conectando").to_owned()),
    }
    if let Some(rtt) = status.rtt_ms {
        facts.push(t!("host-rtt", ms = rtt.round() as u64));
    }
    facts.push(permissions_label(&permissions));
    let mut line = row![
        ui::avatar(&app.theme, &status.name, size::AVATAR),
        column![
            ui::strong(status.name.as_str()),
            ui::secondary(facts.join(" · ")).wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(2)
        .width(Length::Fill),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    // El nivel con lo medido (el RTT de su conexión); sin medida, nada.
    if let Some(rtt) = status.rtt_ms {
        let level = QualityLevel::from_overhead_ms(rtt);
        line = line.push(ui::status_badge(quality_tone(level), quality_label(level)));
    }
    let muted = hears_muted(room, id);
    let kick = ui::with_tooltip(
        button(
            container(icon_with(
                Icon::UserX,
                size::ICON,
                SvgClass::Tone(Tone::Error),
            ))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
        )
        .width(Length::Fixed(size::CONTROL))
        .height(Length::Fixed(size::CONTROL))
        .padding(0)
        .class(ButtonClass::DestructiveFlat)
        .on_press(on(Msg::Kick(id))),
        t!("host-expulsar"),
    );
    line = line
        .push(
            row![
                toggle(
                    Icon::Pointer,
                    permissions.keyboard_mouse,
                    if permissions.keyboard_mouse {
                        t!("host-quitar-teclado-y-raton")
                    } else {
                        t!("host-dar-teclado-y-raton")
                    },
                    on(Msg::Keyboard(id, !permissions.keyboard_mouse)),
                ),
                toggle(
                    Icon::Gamepad,
                    permissions.gamepad_slot.is_some(),
                    if permissions.gamepad_slot.is_some() {
                        t!("host-quitar-el-mando")
                    } else {
                        t!("host-dar-un-mando")
                    },
                    on(Msg::PadOn(id, permissions.gamepad_slot.is_none())),
                ),
                toggle(
                    Icon::Mic,
                    permissions.voice,
                    if permissions.voice {
                        t!("host-quitar-la-voz")
                    } else {
                        t!("host-dar-la-voz")
                    },
                    on(Msg::GuestVoice(id, !permissions.voice)),
                ),
            ]
            .spacing(space::XXS),
        )
        .push(ui::icon_button(
            if muted {
                Icon::HeadphonesOff
            } else {
                Icon::Headphones
            },
            if muted {
                t!("host-volver-a-oir", name = status.name.as_str())
            } else {
                t!("host-dejar-de-oir", name = status.name.as_str())
            },
            Some(on(Msg::Hear(id, muted))),
        ))
        .push(ui::icon_button(
            Icon::Fingerprint,
            t!("host-numero-de-seguridad"),
            Some(on(Msg::Safety(id))),
        ))
        .push(kick);
    let mut card = column![line].spacing(space::XS);
    if permissions.gamepad_slot.is_some() {
        card = card.push(pad_line(app, status));
    }
    if !status.input_key && (permissions.keyboard_mouse || permissions.gamepad_slot.is_some()) {
        card = card.push(
            container(ui::caption(t!("host-esperando-la-clave-de-entrada-pareja"))).padding(
                Padding {
                    left: size::AVATAR + space::S,
                    ..Padding::ZERO
                },
            ),
        );
    }
    container(card)
        .padding(Padding::from([10.0, 16.0]))
        .width(Length::Fill)
        .into()
}

/// El mando de un invitado: la ranura, si va tal cual o convertido, y el mando que declara
/// (nombre, VID:PID y cómo llega) con, si no va tal cual pudiendo, por qué.
fn pad_line<'a>(app: &'a App, status: &'a GuestStatus) -> Element<'a, Message> {
    let id = status.guest;
    let max = coroom_session::input::max_gamepads();
    let slots = pad_choices(app.settings.input.preferred_slot, max);
    let current = PadSlot(status.permissions.gamepad_slot);
    let mut controls = row![
        ui::caption(t!("host-mando")),
        ui::select(slots, Some(current), move |slot| on(Msg::Pad(id, slot))),
        ui::select(
            vec![PadMode(true), PadMode(false)],
            Some(PadMode(status.permissions.gamepad_raw)),
            move |mode| on(Msg::PadMode(id, mode)),
        ),
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    let mut notes = Column::new().spacing(2);
    match &status.gamepad {
        None => {
            notes = notes.push(ui::caption(t!("host-todavia-no-ha-conectado-ningun-mando")));
        }
        Some(pad) => {
            let transport = match pad.transport {
                PadTransport::Hid => "HID",
                PadTransport::XInput => "XInput",
                PadTransport::Converted => t!("host-solo-convertido"),
            };
            controls = controls.push(ui::status_badge(
                Tone::Neutral,
                format!("{:04x}:{:04x} · {transport}", pad.vendor, pad.product),
            ));
            notes = notes.push(ui::caption(t!(
                "host-su-mando-name",
                name = pad.name.as_str()
            )));
            if let Some(note) = &pad.note {
                notes = notes.push(ui::caption(t!(
                    "host-va-convertido-note",
                    note = note.as_str()
                )));
            }
        }
    }
    container(column![controls, notes].spacing(space::XXS))
        .padding(Padding {
            left: size::AVATAR + space::S,
            ..Padding::ZERO
        })
        .into()
}

/// Los invitados, con «Pausar la entrada».
fn guests_section<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let pause = if room.paused {
        ui::button_with(
            Kind::Normal,
            t!("host-reanudar-la-entrada"),
            Some(Icon::Play),
            Some(on(Msg::Pause(false))),
        )
    } else {
        ui::button_with(
            Kind::Normal,
            t!("host-pausar-la-entrada"),
            Some(Icon::Pause),
            (!room.guests.is_empty()).then(|| on(Msg::Pause(true))),
        )
    };
    let head = row![
        ui::strong(t!("host-invitados", valor = room.guests.len())).size(15.0),
        ui::fill(),
        pause
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    let body: Element<'a, Message> = if room.guests.is_empty() {
        ui::card(ui::empty_state(
            Icon::Users,
            t!("host-todavia-no-ha-entrado-nadie"),
            t!("host-comparte-el-codigo-de-la-sala"),
            None,
        ))
    } else {
        ui::boxed_list(
            room.guests
                .iter()
                .map(|status| guest_row(app, room, status))
                .collect(),
        )
    };
    column![head, body].spacing(space::XS).into()
}

/// La calidad con la sala abierta: el segmentado de perfiles y «Avanzado».
fn room_quality(app: &App) -> Element<'_, Message> {
    let head = row![
        ui::strong(t!("host-calidad")).size(15.0),
        ui::fill(),
        preset_segmented(app)
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    column![
        head,
        ui::caption(when_summary()).wrapping(Wrapping::WordOrGlyph),
        advanced(app)
    ]
    .spacing(space::XS)
    .into()
}

/// Amigos conectados que aún no están en la sala, para invitarlos (nada si no hay ninguno).
fn invite_more<'a>(app: &'a App, room: &'a Room) -> Option<Element<'a, Message>> {
    let friends = app.friends.loaded()?;
    let inside: Vec<u64> = room.guests.iter().map(|status| status.user).collect();
    let rows: Vec<Element<'a, Message>> = friends
        .friends
        .iter()
        .filter(|friend| friend.online && !inside.contains(&friend.id))
        .map(|friend| {
            ui::list_row(
                Some(ui::avatar(&app.theme, &friend.display_name, 28.0)),
                friend.display_name.as_str(),
                None,
                vec![
                    ui::status_badge(Tone::Success, t!("host-disponible")),
                    ui::button_with(
                        Kind::Normal,
                        t!("host-invitar"),
                        Some(Icon::Mail),
                        room.code.as_ref().map(|_| on(Msg::Invite(friend.id))),
                    )
                    .into(),
                ],
            )
        })
        .collect();
    if rows.is_empty() {
        return None;
    }
    Some(
        column![
            ui::strong(t!("host-invitar-a-mas-amigos")).size(15.0),
            ui::boxed_list(rows)
        ]
        .spacing(space::XS)
        .into(),
    )
}

/// El chat de la sala, fijo a la derecha (el panel trae su cabecera con «Vaciar el chat»).
fn chat_panel(app: &App) -> Element<'_, Message> {
    container(chat::panel(app, chat::Surface::Window))
        .width(Length::Fixed(CHAT_WIDTH + space::M))
        .height(Length::Fill)
        .padding(Padding {
            top: space::M,
            right: space::M,
            bottom: space::M,
            left: 0.0,
        })
        .into()
}

/// Con la sala abierta.
fn room_view<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let mut page = Column::new().spacing(space::M).push(room_header(app, room));
    if room.paused {
        page = page.push(ui::banner(
            Tone::Warning,
            t!("host-la-entrada-de-todos-los-invitados"),
            Some((t!("host-reanudar"), on(Msg::Pause(false)))),
        ));
    }
    if let Some(relay) = &room.relay {
        page = page.push(ui::banner(
            Tone::Warning,
            t!("host-sin-relay-relay-solo-podran-entrar", relay = relay),
            None,
        ));
    }
    page = page
        .push(room_source_card(app, room))
        .push(guests_section(app, room))
        .push(ui::banner(
            Tone::Info,
            if room.guests_muted {
                t!("host-voz-entran-callados")
            } else {
                t!("host-voz-entran-hablando")
            },
            None,
        ))
        .push(audio_box(app, false))
        .push(room_quality(app));
    if let Some(invite) = invite_more(app, room) {
        page = page.push(invite);
    }
    Row::new()
        .push(container(ui::page(page)).width(Length::Fill))
        .push(chat_panel(app))
        .height(Length::Fill)
        .into()
}

/// Escenas de Compartir para las capturas.
pub(crate) mod shots {
    use coroom_session::latency::InputLatency;
    use coroom_session::remote::{
        CandidateClass, CandidateOutcome, CandidateReport, GuestPad, GuestStatus, PadTransport,
        SessionEvent,
    };
    use coroom_session::{GuestId, Permissions};
    use coroom_transport::quic::Path;

    use super::{Msg, ShareMode, ShareOptions, SourceChoice, TargetOption};
    use crate::ui::app::{App, Dialog, Event, Message, Page};
    use crate::ui::settings::QualityPreset;
    use crate::ui::shots::{Builder, game_frame, playing, safety, signed_in};
    use crate::ui::video::VideoFeed;

    fn monitors(app: &mut App) {
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: vec![coroom_session::capture::CaptureTarget::Monitor {
                id: 1,
                name: "Monitor principal · 2560 × 1440".to_owned(),
            }],
            targets_error: None,
            apps: Ok(playing(&[
                ("juego.exe", "Juego"),
                ("firefox.exe", "Firefox"),
                ("discord.exe", "Discord"),
            ])),
        })));
    }

    fn windows(app: &mut App) {
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: vec![
                coroom_session::capture::CaptureTarget::Window {
                    id: 7,
                    title: "Juego".to_owned(),
                    app: Some("juego.exe".to_owned()),
                },
                coroom_session::capture::CaptureTarget::Window {
                    id: 8,
                    title: "Firefox".to_owned(),
                    app: Some("firefox.exe".to_owned()),
                },
            ],
            targets_error: None,
            apps: Ok(playing(&[
                ("juego.exe", "Juego"),
                ("firefox.exe", "Firefox"),
                ("discord.exe", "Discord"),
            ])),
        })));
    }

    /// Compartir la pantalla, con Bea marcada para invitarla.
    pub(crate) fn host_setup(app: &mut App) {
        signed_in(app, Page::Host);
        app.settings.audio.excluded_apps = vec![crate::ui::settings::RememberedApp {
            key: "discord.exe".to_owned(),
            name: "Discord".to_owned(),
        }];
        monitors(app);
        app.update(Message::Host(Msg::Mark(2, true)));
        app.toasts.clear();
    }

    /// Una ventana, con dos amigos marcados.
    pub(crate) fn host_setup_window(app: &mut App) {
        signed_in(app, Page::Host);
        app.update(Message::Host(Msg::Mode(ShareMode::Window)));
        windows(app);
        app.update(Message::Host(Msg::Mark(2, true)));
        app.update(Message::Host(Msg::Mark(3, true)));
        app.toasts.clear();
    }

    /// Un juego, con «Avanzado» desplegado y el perfil personalizado.
    pub(crate) fn host_setup_advanced(app: &mut App) {
        signed_in(app, Page::Host);
        app.update(Message::Host(Msg::Mode(ShareMode::Game)));
        windows(app);
        app.update(Message::Host(Msg::Preset(QualityPreset::MinLatency)));
        app.update(Message::Host(Msg::Fps(120)));
        app.update(Message::Host(Msg::Advanced(true)));
        app.toasts.clear();
    }

    /// Como en Wayland: sin lista de monitores; lo elegido en el selector del sistema.
    pub(crate) fn host_setup_wayland(app: &mut App) {
        signed_in(app, Page::Host);
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(playing(&[
                ("juego", "Juego"),
                ("firefox", "Firefox"),
                ("discord", "Discord"),
            ])),
        })));
        app.update(Message::Event(Event::SharePicked(
            SourceChoice::Screen,
            Ok(Some(coroom_session::capture::PickedShare {
                target: coroom_session::capture::CaptureTarget::Monitor {
                    id: 0,
                    name: "2560 × 1440".to_owned(),
                },
                restore_token: None,
            })),
        )));
        app.toasts.clear();
    }

    pub(crate) fn guest_status(
        id: u32,
        name: &str,
        permissions: Permissions,
        rtt: f64,
    ) -> GuestStatus {
        let relayed = id != 1;
        GuestStatus {
            guest: GuestId(id),
            user: u64::from(id) + 1,
            name: name.to_owned(),
            public_key: [id as u8; 32],
            fingerprint: format!("{:02x}", id + 0xb0).repeat(32),
            safety_number: safety(),
            permissions,
            leaf: id,
            path: Some(if relayed { Path::Relayed } else { Path::Direct }),
            // Direcciones de documentación (RFC 5737), no de nadie.
            remote: Some(if relayed {
                "198.51.100.7:4433".parse().expect("dirección")
            } else {
                "192.0.2.24:50412".parse().expect("dirección")
            }),
            rtt_ms: Some(rtt),
            target_kbps: Some(if relayed { 9_500 } else { 18_400 }),
            link_kbps: Some(if relayed { 21_000 } else { 48_000 }),
            // Por el relay: ninguna dirección directa respondió (CGNAT).
            candidates: if relayed {
                vec![CandidateReport {
                    addr: "198.51.100.40:50000".parse().expect("dirección"),
                    class: CandidateClass::Ipv4Public,
                    outcome: CandidateOutcome::NoResponse,
                }]
            } else {
                Vec::new()
            },
            input_key: true,
            input: InputLatency::default(),
            gamepad: None,
        }
    }

    pub(crate) fn with_pad(status: GuestStatus, pad: GuestPad) -> GuestStatus {
        GuestStatus {
            gamepad: Some(pad),
            ..status
        }
    }

    /// La sala abierta con dos invitados: Bea directa y Carla por el relay, con mando.
    pub(crate) fn host_room(app: &mut App) {
        host_setup(app);
        let preview = VideoFeed::detached();
        preview.push(game_frame(1, 640, 360), None, None);
        app.update(Message::Host(Msg::Start));
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
                    gamepad_raw: true,
                },
                14.2,
            ),
            with_pad(
                guest_status(
                    2,
                    "Carla",
                    Permissions {
                        keyboard_mouse: false,
                        gamepad_slot: Some(1),
                        voice: false,
                        gamepad_raw: true,
                    },
                    61.4,
                ),
                GuestPad {
                    name: "8BitDo Ultimate 2 Wireless Controller".to_owned(),
                    vendor: 0x2dc8,
                    product: 0x6012,
                    transport: PadTransport::Hid,
                    raw: true,
                    note: None,
                },
            ),
        ])));
        app.toasts.clear();
    }

    /// Con la sala abierta, eligiendo un juego en vez de la pantalla.
    pub(crate) fn host_room_switch(app: &mut App) {
        host_room(app);
        app.update(Message::Host(Msg::ChangeSource(true)));
        app.update(Message::Host(Msg::Mode(ShareMode::Game)));
        // Un título largo, como los de Windows: no debe aplastar el selector.
        let game = coroom_session::capture::CaptureTarget::Window {
            id: 7,
            title: "Fatal Fury: City of the Wolves — Edición especial (DirectX 12)".to_owned(),
            app: Some("juego.exe".to_owned()),
        };
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: vec![game.clone()],
            targets_error: None,
            apps: Ok(playing(&[
                ("juego.exe", "Juego"),
                ("firefox.exe", "Firefox"),
                ("discord.exe", "Discord"),
            ])),
        })));
        app.update(Message::Host(Msg::Target(TargetOption(game))));
        app.toasts.clear();
    }

    pub(crate) fn host_stop_dialog(app: &mut App) {
        host_room(app);
        app.dialog = Some(Dialog::StopSharing);
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[
        ("08-compartir", host_setup),
        ("08-compartir-ventana", host_setup_window),
        ("08-compartir-avanzado", host_setup_advanced),
        ("08-compartir-wayland", host_setup_wayland),
        ("09-sala-abierta", host_room),
        ("09-sala-cambiar-fuente", host_room_switch),
        ("10-dejar-de-compartir", host_stop_dialog),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;
    use coroom_transport::proto::api::{Friend, Friends};

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
            remote: None,
            rtt_ms: None,
            target_kbps: None,
            link_kbps: None,
            candidates: Vec::new(),
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

    fn friend(id: u64, name: &str, online: bool) -> Friend {
        Friend {
            id,
            username: name.to_lowercase(),
            display_name: name.to_owned(),
            online,
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
        assert!(effects.is_empty(), "se guarda al poco: {effects:?}");
        assert_eq!(
            app.flush_settings(),
            Some(Effect::SaveSettings),
            "se recuerda"
        );
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
                guests_muted: false,
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
    fn sharing_a_game_sends_only_its_audio() {
        let mut app = app();
        app.update(Message::Navigate(Page::Host));
        assert_eq!(
            app.update(Message::Host(Msg::Mode(ShareMode::Game))),
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
                voice: app.settings.audio.host_voice,
                guests_muted: false,
            })
        );
    }

    /// Ventana y Juego capturan lo mismo: cambiar entre ellos solo cambia el audio, sin volver a
    /// mirar qué se puede compartir.
    #[test]
    fn a_window_sends_every_app_and_a_game_only_its_own() {
        let mut app = app();
        app.update(Message::Host(Msg::Mode(ShareMode::Window)));
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: vec![window(7, "Juego", Some("juego.exe"))],
            targets_error: None,
            apps: Ok(playing(&["juego.exe", "discord.exe"])),
        })));
        assert_eq!(
            app.host.game_capture(&[]),
            GameCapture::Apps(AppFilter::AllExcept(Vec::new()))
        );
        assert!(
            app.update(Message::Host(Msg::Mode(ShareMode::Game)))
                .is_empty(),
            "la misma lista de ventanas"
        );
        assert_eq!(
            app.host.game_capture(&[]),
            GameCapture::Apps(AppFilter::Only(vec!["juego.exe".into()]))
        );
    }

    #[test]
    fn the_voice_switches_are_the_settings() {
        let mut app = app();
        app.screen = crate::ui::app::Screen::Main;
        app.update(Message::Navigate(Page::Host));
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::Screen,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(Vec::new()),
        })));
        assert!(
            !app.settings.room.guests_muted,
            "por defecto entran hablando"
        );
        assert!(app.update(Message::Host(Msg::GuestsMuted(true))).is_empty());
        app.update(Message::Host(Msg::Voice(false)));
        assert!(app.settings.room.guests_muted && !app.settings.audio.host_voice);
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        match app.update(Message::Host(Msg::Start)).as_slice() {
            [Effect::StartHost(request)] => assert!(request.guests_muted && !request.voice),
            other => panic!("se esperaba abrir la sala: {other:?}"),
        }
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        assert_eq!(
            app.host.room.as_ref().map(|room| room.guests_muted),
            Some(true),
            "el aviso de la sala dice cómo entran"
        );
    }

    #[test]
    fn without_window_owners_the_likely_game_is_marked() {
        let mut app = app();
        app.update(Message::Host(Msg::Mode(ShareMode::Game)));
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
        app.update(Message::Host(Msg::Mode(ShareMode::Window)));
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
    fn permissions_are_toggles_and_slots_are_exclusive() {
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
        // El conmutador del mando da la primera ranura libre (la preferida, si lo está).
        let effects = app.update(Message::Host(Msg::PadOn(GuestId(2), true)));
        let [Effect::SetPermissions(GuestId(2), p)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert!(p.gamepad_slot.is_some() && p.gamepad_slot != Some(2));
        let effects = app.update(Message::Host(Msg::PadOn(GuestId(2), false)));
        assert!(matches!(&effects[..], [Effect::SetPermissions(_, p)] if p.gamepad_slot.is_none()));
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
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(
            app.flush_settings(),
            Some(Effect::SaveSettings),
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

    /// Los amigos marcados se invitan cuando el servidor crea la sala (antes no hay código), y
    /// solo los que siguen conectados.
    #[test]
    fn marked_friends_are_invited_once_the_room_exists() {
        let mut app = app();
        app.update(Message::Event(Event::Friends(Ok(Friends {
            friends: vec![
                friend(2, "Bea", true),
                friend(3, "Carla", true),
                friend(4, "Dani", false),
            ],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }))));
        assert_eq!(app.host.friends_at, Some(app.now), "en vivo · hace 0 s");
        app.update(Message::Host(Msg::Mark(2, true)));
        app.update(Message::Host(Msg::Mark(3, true)));
        app.update(Message::Host(Msg::Mark(3, false)));
        app.update(Message::Host(Msg::Mark(4, true)));
        assert_eq!(marked_count(&app), 1, "Dani está desconectado");
        let effects = app.update(Message::Host(Msg::Start));
        assert!(!effects.contains(&Effect::Invite(2)), "aún sin sala");
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        let effects = app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "K7Q2MZ".into(),
            fingerprint: "cd".repeat(32),
        })));
        assert_eq!(effects, [Effect::Invite(2)]);
        assert!(app.toasts.iter().any(|t| t.text.contains("Bea")));
        assert!(app.host.invite.is_empty(), "una sola vez");
    }

    /// Elegir una tarjeta aplica el perfil y lo guarda; sin sala no hay efectos de vídeo.
    #[test]
    fn choosing_a_preset_applies_it_and_saves() {
        let mut app = app();
        assert_eq!(app.settings.video.preset, QualityPreset::Balanced);
        let effects = app.update(Message::Host(Msg::Preset(QualityPreset::MinLatency)));
        assert!(effects.is_empty(), "sin sala, nada en vivo: {effects:?}");
        let video = &app.settings.video;
        assert_eq!(video.preset, QualityPreset::MinLatency);
        assert_eq!(
            (video.max_resolution, video.fps, video.max_bitrate_kbps),
            (Resolution::P1080, 60, 10_000)
        );
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        app.update(Message::Host(Msg::Preset(QualityPreset::MaxQuality)));
        assert_eq!(app.settings.video.max_bitrate_kbps, UNLIMITED_KBPS);
        assert!(app.settings.video.chroma_444);
        assert!(app.toasts.is_empty(), "sin sala no hay «próxima sala»");
    }

    /// Tocar algo en Avanzado pasa el perfil a «Personalizado» (repetir el valor no).
    #[test]
    fn touching_advanced_makes_the_preset_custom() {
        let mut app = app();
        app.update(Message::Host(Msg::Fps(60)));
        assert_eq!(
            app.settings.video.preset,
            QualityPreset::Balanced,
            "sin cambio"
        );
        for msg in [
            Msg::Fps(120),
            Msg::Resolution(pick(Resolution::P1440, "1440p")),
            Msg::Codec(pick(CodecChoice::Hevc, "HEVC")),
            Msg::Encoder(pick(EncoderChoice::Software, "software")),
            Msg::Capped(false),
            Msg::Chroma444(true),
            Msg::Hdr(true),
        ] {
            app.update(Message::Host(Msg::Preset(QualityPreset::Balanced)));
            assert!(app.update(Message::Host(msg.clone())).is_empty(), "{msg:?}");
            assert_eq!(app.settings.video.preset, QualityPreset::Custom, "{msg:?}");
        }
        // El tope: quitarlo es «sin tope»; volver a ponerlo recuerda el último.
        app.update(Message::Host(Msg::Cap(pick(30_000, "30"))));
        app.update(Message::Host(Msg::Capped(false)));
        assert_eq!(app.settings.video.max_bitrate_kbps, UNLIMITED_KBPS);
        app.update(Message::Host(Msg::Capped(true)));
        assert_eq!(app.settings.video.max_bitrate_kbps, 30_000);
    }

    /// Con la sala abierta, lo «En vivo» se aplica ya y lo «Al abrir la sala» solo se guarda con
    /// un aviso.
    #[test]
    fn with_the_room_open_live_options_apply_and_the_rest_wait() {
        let mut app = app();
        open_room(&mut app);
        app.toasts.clear();
        assert_eq!(
            app.update(Message::Host(Msg::Codec(pick(CodecChoice::Av1, "AV1")))),
            [Effect::SetCodec]
        );
        assert_eq!(
            app.update(Message::Host(Msg::Cap(pick(10_000, "10")))),
            [Effect::SetMaxBitrate(10_000)]
        );
        assert!(app.toasts.is_empty(), "lo en vivo no avisa de nada");
        assert!(app.update(Message::Host(Msg::Fps(30))).is_empty());
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text == "Guardado para la próxima sala")
        );
        // Un perfil mezcla las dos cosas: el tope va ya, el resto espera.
        app.toasts.clear();
        assert_eq!(
            app.update(Message::Host(Msg::Preset(QualityPreset::MaxQuality))),
            [Effect::SetMaxBitrate(UNLIMITED_KBPS)]
        );
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
    }

    /// Las etiquetas salen de [`Video::when`].
    #[test]
    fn the_when_summary_comes_from_the_settings_model() {
        let summary = when_summary();
        assert!(summary.starts_with("En vivo: "), "{summary}");
        assert!(summary.contains("Al abrir la sala: "), "{summary}");
        assert_eq!(bitrate_phrase(20_000), "Auto hasta 20 Mbit/s");
        assert_eq!(bitrate_phrase(UNLIMITED_KBPS), "Auto sin tope");
        let mut video = Video::default();
        QualityPreset::MinLatency.apply(&mut video);
        assert_eq!(video_spec(&video), "1080p · 60 fps · Auto hasta 10 Mbit/s");
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
        app.update(Message::Host(Msg::Mode(ShareMode::Game)));
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
    fn with_the_room_open_the_source_and_audio_change_live() {
        let mut app = app();
        app.screen = crate::ui::app::Screen::Main;
        app.update(Message::Host(Msg::Mode(ShareMode::Game)));
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
        app.update(Message::Host(Msg::ChangeSource(true)));
        assert!(app.host.changing_source);
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
        assert!(!app.host.changing_source, "el selector se pliega");
        assert_eq!(
            app.host.shared.as_ref().and_then(|s| s.target.clone()),
            Some(other.clone())
        );

        // Si no arranca, sigue lo de antes y se avisa.
        app.update(Message::Host(Msg::ChangeSource(true)));
        app.update(Message::Host(Msg::Target(TargetOption(game))));
        app.update(Message::Host(Msg::ApplySource));
        app.update(Message::Event(Event::SourceSwitched(Err("no".into()))));
        assert!(app.host.switching.is_none());
        assert!(
            app.host.source_changed(),
            "lo elegido sigue sin transmitirse"
        );
        // Cancelar vuelve a lo que se transmite.
        app.update(Message::Host(Msg::ChangeSource(false)));
        assert!(!app.host.source_changed());
        assert_eq!(app.host.target, Some(other));

        // Juego → Ventana con lo mismo: solo cambia el audio, en vivo.
        let effects = app.update(Message::Host(Msg::Mode(ShareMode::Window)));
        assert_eq!(
            effects,
            [Effect::SetAppAudio(GameCapture::Apps(
                AppFilter::AllExcept(Vec::new())
            ))]
        );
    }
}
