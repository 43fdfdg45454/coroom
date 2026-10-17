//! Pantalla «Compartir» (anfitrión), según el boceto aprobado.
//!
//! Antes de abrir la sala: arriba, **Qué compartir** (Pantalla o Ventana, las dos con el audio de
//! todas las aplicaciones salvo las quitadas) con el monitor o la ventana, y **A quién invitar**
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
//! Con la sala abierta (la misma página): la cabecera con el código (que cae letra a letra al
//! crearse la sala), los invitados y la ruta; arriba, **la transmisión**: lo que se comparte con
//! borde rojo y la etiqueta «EN VIVO» con el tiempo transmitiendo (brillan una vez al empezar y
//! quedan quietos), «Cambiar fuente» (el mismo selector, que se abre con su altura) y «Dejar de
//! compartir»; o «Solo voz» si no se comparte nada. Debajo, **Personas**: una fila de altura fija
//! por persona (avatar con anillo de voz, nombre y estado, «Hablando» en su ranura y la latencia
//! siempre visible) con su desplegable (volumen, permisos, mando, número de seguridad, pasar la
//! sala y expulsar), que solo se abre o se cierra con un clic; y la **bandeja** de los amigos que
//! piden entrar ([`JoinRequest`], que caen al llegar, muestran el tiempo que les queda y se
//! pliegan a los [`JOIN_REQUEST_TTL`]). Después, el audio, el micrófono y la salida, la calidad
//! y, a la derecha, el chat de la sala.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use coroom_session::audio::GameCapture;
use coroom_session::audio::apps::{AppFilter, PlayingApp, normalize};
use coroom_session::capture::{CaptureTarget, PickedShare, ShareChoice, ShareKind};
use coroom_session::remote::{
    CandidateOutcome, GuestStatus, PadTransport, QualityLevel, SessionEvent, permissions_label,
};
use coroom_session::{GuestId, Permissions};
use coroom_ui::Element;
use coroom_ui::animate::{Collapse, Shift};
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::motion::{EXIT_EASING, Motion, Reveal, Token};
use coroom_ui::sound::{Cue, cue};
use coroom_ui::style::{
    ButtonClass, ContainerClass, ProgressClass, SliderClass, SvgClass, TextClass,
};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::alignment::Horizontal;
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding, Radians, Rotation, Vector};
use iced_widget::{Column, Row, button, checkbox, column, container, progress_bar, row, rule};

use super::settings::Applies;
use super::{chat, settings};
use crate::portal::RestoreTokens;
use crate::ui::app::{
    App, AudioDevice, Dialog, Effect, Event, HostRequest, HostVoice, JOIN_REQUEST_TTL, Load,
    Message, Page, Screen, quality_tone,
};
use crate::ui::avatars;
use crate::ui::settings::{
    CodecChoice, DEFAULT_VOICE_VOLUME, EncoderChoice, FPS_CHOICES, KnownPeer, MAX_VOICE_VOLUME,
    QualityPreset, RememberedApp, Resolution, Video, VideoOption, When, voice_gain,
};
use crate::ui::stats;
use crate::ui::video::{VideoFeed, video_view};

/// Cada cuánto se vuelve a mirar qué suena y qué se puede compartir.
pub const REFRESH: Duration = Duration::from_secs(2);

/// Ancho del panel del chat con la sala abierta.
const CHAT_WIDTH: f32 = 320.0;

/// Ancho de la miniatura de la fuente antes de abrir la sala (16:9).
const THUMB_WIDTH: f32 = 160.0;

/// Ancho de la vista previa con la sala abierta (16:9).
const PREVIEW_WIDTH: f32 = 192.0;

/// Lo que dura el tilde de «Copiar el código» antes de volver a ser el icono de copiar.
const COPIED: Duration = Duration::from_millis(1_500);

/// Alto de la barra del tiempo que le queda a una petición de entrada.
const TIME_BAR: f32 = 3.0;

/// Lo que sube la tarjeta de calidad elegida.
const LIFT: f32 = 2.0;

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
    /// Nada: la sala es solo de voz y chat.
    Nothing,
}

impl SourceChoice {
    /// Qué se comparte (la fuente de prueba y nada, como la pantalla).
    pub fn kind(self) -> ShareKind {
        match self {
            SourceChoice::App => ShareKind::App,
            SourceChoice::Screen | SourceChoice::Synthetic | SourceChoice::Nothing => {
                ShareKind::Screen
            }
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
    /// La fuente de prueba (un patrón animado).
    Test,
    /// Nada: la sala es solo de voz y chat (se puede compartir después, desde la sala).
    Nothing,
}

impl ShareMode {
    /// Los del segmentado (la fuente de prueba va aparte, como enlace).
    pub const PICKER: [ShareMode; 3] = [ShareMode::Screen, ShareMode::Window, ShareMode::Nothing];

    /// Fuente que se captura.
    pub fn source(self) -> SourceChoice {
        match self {
            ShareMode::Screen => SourceChoice::Screen,
            ShareMode::Window => SourceChoice::App,
            ShareMode::Test => SourceChoice::Synthetic,
            ShareMode::Nothing => SourceChoice::Nothing,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ShareMode::Screen => t!("host-modo-pantalla"),
            ShareMode::Window => t!("host-modo-ventana"),
            ShareMode::Test => t!("host-la-fuente-de-prueba"),
            ShareMode::Nothing => t!("host-modo-solo-voz"),
        }
    }

    fn help(self) -> &'static str {
        match self {
            ShareMode::Screen => t!("host-pantalla-ayuda"),
            ShareMode::Window => t!("host-ventana-ayuda"),
            ShareMode::Test => t!("host-fuente-de-prueba-ayuda"),
            ShareMode::Nothing => t!("host-solo-voz-ayuda"),
        }
    }

    fn icon(self) -> Icon {
        match self {
            ShareMode::Screen => Icon::Monitor,
            ShareMode::Window => Icon::AppWindow,
            ShareMode::Test => Icon::Activity,
            ShareMode::Nothing => Icon::Mic,
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

/// Una opción con su texto.
pub fn pick<T>(value: T, label: impl Into<String>) -> Pick<T> {
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
    /// Volumen de la voz de cada invitado en este equipo (porcentaje; sin entrada, 100).
    pub volumes: BTreeMap<GuestId, u8>,
    /// Si los invitados de esta sala entran callados.
    pub guests_muted: bool,
    /// La voz propia en la sala (micrófono y quién habla), con cada lista de invitados.
    pub voice: HostVoice,
    /// El silencio propio que se acaba de pedir, hasta que una lista lo confirme: la lista que
    /// llega justo después del clic puede haberse tomado antes y no debe deshacerlo en pantalla.
    pub mute_pending: Option<bool>,
}

impl Room {
    /// Volumen de la voz del invitado `id` en este equipo (porcentaje).
    pub fn volume(&self, id: GuestId) -> u8 {
        self.volumes
            .get(&id)
            .copied()
            .unwrap_or(DEFAULT_VOICE_VOLUME)
    }
}

/// Una acción sobre un invitado desde su desplegable (lo que puede hacer y quitarle el anfitrión).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestAction {
    /// Dar (`true`) o quitar el teclado y el ratón.
    Keyboard(bool),
    /// Dar o quitar un mando.
    Pad(bool),
    /// Dar o quitar la voz.
    Voice(bool),
    /// Ver el número de seguridad.
    Safety,
    /// Pasarle la sala (pide confirmación).
    Handoff,
    /// Expulsar (pide confirmación).
    Kick,
}

/// Un amigo que pide entrar en la sala abierta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRequest {
    /// Su usuario.
    pub user: u64,
    /// Su nombre visible.
    pub name: String,
    /// Cuándo lo pidió.
    pub at: Instant,
}

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Qué se comparte (pantalla, ventana o la fuente de prueba).
    pub mode: ShareMode,
    /// Monitor o ventana elegido (si el sistema deja elegirlo aquí).
    pub target: Option<CaptureTarget>,
    /// Lo elegido en el selector del sistema (Wayland), para la fuente actual.
    pub picked: Option<PickedShare>,
    /// Tokens con los que el portal de captura repite la elección sin preguntar, por tipo de
    /// fuente (`portal.json`; los carga la ventana al arrancar y los guarda
    /// [`Effect::SaveRestoreTokens`]).
    pub restore_tokens: RestoreTokens,
    /// Esperando al selector del sistema.
    pub picking: bool,
    /// Lo que se puede compartir (`None`: aún no se ha mirado).
    pub options: Option<ShareOptions>,
    /// Mirando qué se puede compartir.
    pub refreshing: bool,
    /// Cuándo se miró por última vez.
    pub refreshed_at: Option<Instant>,
    /// Lista de audio desplegada.
    pub audio_open: bool,
    /// Amigos marcados para invitar al abrir la sala.
    pub invite: BTreeSet<u64>,
    /// Cuándo llegó la última lista de amigos o un cambio de presencia.
    pub friends_at: Option<Instant>,
    /// «Avanzado» desplegado.
    pub advanced: bool,
    /// Abriendo.
    pub starting: bool,
    /// Desde cuándo (anima la barra de «Abriendo la sala…»).
    pub starting_since: Option<Instant>,
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
    /// Amigos que piden entrar en la sala abierta, en el orden en que lo pidieron.
    pub join_requests: Vec<JoinRequest>,
    /// Pasando la sala: a quién se le ofreció (esperando su respuesta).
    pub handoff: Option<(GuestId, String)>,
    /// Pasando la sala: aceptó y se manda a todos a la suya (ya no se puede retirar).
    pub handing_over: bool,
    /// Pasando la sala: el código de la sala nueva, para entrar en ella al cerrar esta.
    pub moving_to: Option<String>,
    /// Como invitado, la sala que pasa el anfitrión: en qué punto va.
    pub takeover: Takeover,
    /// Lo que se anima en la sala: los momentos, los desplegables y lo que llega o se va.
    pub motion: RoomMotion,
}

/// Quién es una fila de «Personas»: el anfitrión (este equipo) o un invitado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Person {
    /// El anfitrión.
    Me,
    /// Un invitado.
    Guest(GuestId),
}

/// Un desplegable: abierto o no, con su animación de altura ([`Token::Panel`]; la salida al 70 %).
/// Solo cambia por una acción de quien usa la aplicación ([`Msg::Expand`], [`Msg::ChangeSource`]),
/// nunca por un dato que llega.
#[derive(Debug, Clone)]
pub struct Fold(Reveal);

impl Default for Fold {
    fn default() -> Self {
        Self(Reveal::new(Motion::FULL, Token::Panel, false))
    }
}

impl Fold {
    /// Si está abierto (o abriéndose).
    pub fn is_open(&self) -> bool {
        self.0.is_shown()
    }

    /// Abre o cierra desde `at`, con el movimiento de ahora.
    fn set(&mut self, open: bool, motion: Motion, at: Instant) {
        self.0.set_motion(motion);
        self.0.set(open, at);
    }

    /// Lo abierto en `at`: 0 cerrado, 1 abierto, con su curva.
    fn shown(&self, at: Instant) -> f32 {
        self.0.progress(at)
    }

    fn animating(&self, at: Instant) -> bool {
        self.0.is_animating(at)
    }

    /// El mismo estado, sin animación en curso.
    fn at_rest(&self) -> Self {
        Self(Reveal::new(Motion::OFF, Token::Panel, self.is_open()))
    }
}

/// Un invitado que acaba de irse: su fila se pliega en el sitio donde estaba.
#[derive(Debug, Clone)]
pub struct LeavingGuest {
    /// Cómo estaba.
    pub status: GuestStatus,
    /// En qué puesto de la lista.
    pub index: usize,
    /// Desde cuándo se pliega.
    pub since: Instant,
}

/// Una petición de entrada que se va de la bandeja (caducó o se contestó): se pliega.
#[derive(Debug, Clone)]
pub struct LeavingRequest {
    /// La petición.
    pub request: JoinRequest,
    /// Desde cuándo se pliega.
    pub since: Instant,
}

/// Las marcas de tiempo de lo que se anima en la sala. Todo se dibuja en función de `App::now`
/// (los cuadros los pide la ventana mientras [`animating`] lo diga); con la transmisión en marcha
/// nada se anima de continuo: el tiempo «EN VIVO» cambia una vez por segundo con un `Tick`.
#[derive(Debug, Clone, Default)]
pub struct RoomMotion {
    /// Cuándo se abrió la sala: la vista previa crece hasta su sitio ([`Token::Moment`]).
    pub opened_at: Option<Instant>,
    /// Cuándo llegó el código: cae letra a letra.
    pub code_at: Option<Instant>,
    /// Cuándo se copió el código: el icono es un tilde durante [`COPIED`].
    pub copied_at: Option<Instant>,
    /// Desde cuándo se transmite algo (el tiempo de «EN VIVO»).
    pub live_since: Option<Instant>,
    /// Cuándo empezó a transmitirse: el borde y la etiqueta brillan una vez.
    pub glow_at: Option<Instant>,
    /// Cuándo se dejó de compartir: el rojo se apaga con un fundido.
    pub live_ended: Option<Instant>,
    /// «Cambiar fuente».
    pub source: Fold,
    /// El desplegable de cada persona.
    pub people: BTreeMap<Person, Fold>,
    /// Cuándo llegó cada invitado: su fila entra deslizándose.
    pub arrived: BTreeMap<GuestId, Instant>,
    /// Quienes acaban de irse.
    pub leaving: Vec<LeavingGuest>,
    /// Peticiones de entrada que se van.
    pub requests_leaving: Vec<LeavingRequest>,
    /// El perfil de calidad de antes y cuándo se cambió: la tarjeta elegida se eleva.
    pub preset: Option<(QualityPreset, Instant)>,
    /// Cuándo se dejó todo en reposo ([`settle`]): las peticiones de entrada de antes ya están
    /// dentro (su hora de llegada sigue contando para que caduquen).
    pub settled: Option<Instant>,
}

impl RoomMotion {
    /// Lo que lleva de su entrada una petición de entrar que llegó en `at`.
    fn request_entered(&self, app: &App, at: Instant) -> f32 {
        if self.settled.is_some_and(|settled| at <= settled) {
            1.0
        } else {
            progress(app, at, Token::Panel, Duration::ZERO)
        }
    }
}

/// Como invitado, la sala que el anfitrión pasa a este equipo ([`accept_handoff`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Takeover {
    /// Nada.
    #[default]
    None,
    /// Aceptada: abriendo la sala propia; con su código se le contesta.
    Opening,
    /// Contestado con el código: se sigue en la del anfitrión hasta que confirme
    /// (`MoveToRoom` con este código) o la cierre, para que la respuesta no se pierda al salir.
    Replied(String),
    /// El anfitrión retiró la oferta mientras se abría la sala propia: se cierra al abrirse.
    Withdrawn,
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
    /// Si se captura lo mismo (el título de una ventana puede cambiar sin que cambie la
    /// ventana).
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
            // El último token que dio el portal para esta fuente (lo renueva en cada arranque) y,
            // si no hay ninguno guardado, el del selector.
            restore_token: self
                .restore_tokens
                .get(self.source().kind())
                .map(str::to_owned)
                .or_else(|| {
                    self.picked
                        .as_ref()
                        .and_then(|picked| picked.restore_token.clone())
                }),
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

    /// Filas de la lista de audio, con las aplicaciones quitadas que se recuerdan.
    pub fn audio_rows(&self, excluded: &[RememberedApp]) -> Vec<AudioRow> {
        let playing = self.playing().unwrap_or_default();
        let is_excluded = |key: &str| excluded.iter().any(|app| normalize(&app.key) == key);
        let mut rows: Vec<AudioRow> = playing
            .iter()
            .map(|app| {
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
            })
            .collect();
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
        rows
    }

    /// Audio del juego que se pide al abrir la sala: todas las aplicaciones salvo las quitadas.
    pub fn game_capture(&self, excluded: &[RememberedApp]) -> GameCapture {
        if self.mode == ShareMode::Nothing {
            // Sin nada compartido no hay juego que oír: solo las voces.
            return GameCapture::None;
        }
        if self.playing().is_none() {
            // Sin audio por aplicación en este equipo: todo lo que suena.
            return GameCapture::System;
        }
        GameCapture::Apps(AppFilter::AllExcept(
            excluded.iter().map(|app| normalize(&app.key)).collect(),
        ))
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
    /// Croma 4:4:4.
    Chroma444(bool),
    /// HDR.
    Hdr(bool),
    /// Abrir la sala.
    Start,
    /// Con la sala abierta: dejar de compartir (la sala sigue con voz y chat).
    StopSharing,
    /// Con la sala abierta y sin compartir nada: elegir qué compartir.
    StartSharing,
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
    /// Volumen de la voz de un invitado en este equipo (porcentaje), mientras se arrastra.
    Volume(GuestId, u8),
    /// Al soltar el deslizador: recordar el volumen por el nombre del invitado.
    VolumeDone(GuestId),
    /// Expulsar (pide confirmación).
    Kick(GuestId),
    /// Una acción del menú de la fila de un invitado.
    GuestMenu(GuestId, GuestAction),
    /// Pasarle la sala a un invitado (pide confirmación).
    Handoff(GuestId),
    /// Retirar la oferta de pasar la sala mientras no contesta.
    CancelHandoff,
    /// Volumen de la voz propia que se envía (porcentaje), mientras se arrastra.
    MicVolume(u8),
    /// Al soltar el deslizador de la voz propia: recordarlo en los ajustes.
    MicVolumeDone,
    /// Silenciar o no el micrófono propio para toda la sala.
    SelfMute(bool),
    /// Número de seguridad de un invitado.
    Safety(GuestId),
    /// Invitar a un amigo conectado con la sala abierta.
    Invite(u64),
    /// Dejar entrar a quien lo pidió (se le invita).
    AllowJoin(u64),
    /// No dejar entrar ahora a quien lo pidió.
    DeclineJoin(u64),
    /// Con la sala abierta: desplegar (o cancelar) el selector de fuente.
    ChangeSource(bool),
    /// Con la sala abierta: compartir lo elegido en vez de lo de ahora.
    ApplySource,
    /// Abrir o cerrar el desplegable de una persona.
    Expand(Person),
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
    if before.chroma_444 != after.chroma_444 {
        changed.push(VideoOption::Chroma444);
    }
    if before.hdr != after.hdr {
        changed.push(VideoOption::Hdr);
    }
    changed
}

/// Cambia el vídeo de los ajustes y lo guarda; con la sala abierta, aplica lo que va en vivo
/// ([`Video::when`]: el códec, la resolución máxima y los fps juntos, con un solo
/// [`Effect::SetEncoding`] que reabre el codificador) y avisa de lo que espera a
/// la próxima sala. `custom`: el cambio viene de «Avanzado» y el perfil pasa a «Personalizado».
fn change_video(app: &mut App, custom: bool, change: impl FnOnce(&mut Video)) -> Vec<Effect> {
    let before = app.settings.video.clone();
    change(&mut app.settings.video);
    let changed = changed_options(&before, &app.settings.video);
    if custom && !changed.is_empty() {
        app.settings.video.preset = QualityPreset::Custom;
    }
    if before.preset != app.settings.video.preset {
        // La tarjeta elegida se eleva y la de antes baja.
        app.host.motion.preset = Some((before.preset, app.now));
    }
    let mut effects = app.settings_changed();
    if app.host.room.is_none() {
        return effects;
    }
    let mut next_room = false;
    for option in changed {
        match (Video::when(option), option) {
            (When::Live, VideoOption::Codec | VideoOption::Resolution | VideoOption::Fps) => {
                if !effects.contains(&Effect::SetEncoding) {
                    let video = &app.settings.video;
                    tracing::info!(
                        codec = ?video.codec,
                        encoder = ?video.encoder,
                        resolution = ?video.max_resolution,
                        fps = video.fps,
                        "codec, resolucion y fps para la sala abierta"
                    );
                    effects.push(Effect::SetEncoding);
                }
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
        Msg::Target(TargetOption(target)) => app.host.target = Some(target),
        Msg::AppAudio(key, name, on) => {
            let key = normalize(&key);
            // Quitar una aplicación se recuerda.
            let excluded = &mut app.settings.audio.excluded_apps;
            excluded.retain(|known| normalize(&known.key) != key);
            if !on {
                excluded.push(RememberedApp { key, name });
            }
            let mut effects = app.settings_changed();
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
        Msg::Chroma444(on) => return change_video(app, true, |video| video.chroma_444 = on),
        Msg::Hdr(on) => return change_video(app, true, |video| video.hdr = on),
        Msg::Start => return start(app),
        Msg::StopSharing => {
            let host = &mut app.host;
            if host.room.is_none() || host.switching.is_some() {
                return Vec::new();
            }
            host.mode = ShareMode::Nothing;
            host.target = None;
            host.picked = None;
            set_changing(app, false);
            return apply_source(app);
        }
        Msg::StartSharing => {
            if app.host.room.is_none() {
                return Vec::new();
            }
            if !app.host.changing_source {
                cue(Cue::Open);
            }
            set_changing(app, true);
            return set_mode(app, ShareMode::Screen);
        }
        Msg::CopyCode => {
            if let Some(code) = app.host.room.as_ref().and_then(|room| room.code.clone()) {
                app.host.motion.copied_at = Some(app.now);
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
                let mut effects = vec![Effect::SetVoiceMuted {
                    host: true,
                    source: id.0,
                    muted: !hear,
                }];
                // Se recuerda por su nombre, como el volumen: vale si vuelve a entrar.
                if let Some(status) = guest(app, id) {
                    let name = status.name.clone();
                    app.settings.audio.set_voice_muted(&name, !hear);
                    effects.extend(app.settings_changed());
                }
                return effects;
            }
        }
        Msg::Volume(id, percent) => {
            if let Some(room) = app.host.room.as_mut() {
                let percent = percent.min(MAX_VOICE_VOLUME);
                room.volumes.insert(id, percent);
                return vec![Effect::SetVoiceGain {
                    host: true,
                    source: id.0,
                    gain: voice_gain(percent),
                }];
            }
        }
        Msg::VolumeDone(id) => {
            if let Some(room) = app.host.room.as_ref()
                && let Some(status) = guest(app, id)
            {
                let name = status.name.clone();
                app.settings.audio.set_voice_volume(&name, room.volume(id));
                return app.settings_changed();
            }
        }
        Msg::GuestMenu(id, action) => {
            return update(
                app,
                match action {
                    GuestAction::Keyboard(on) => Msg::Keyboard(id, on),
                    GuestAction::Pad(on) => Msg::PadOn(id, on),
                    GuestAction::Voice(on) => Msg::GuestVoice(id, on),
                    GuestAction::Safety => Msg::Safety(id),
                    GuestAction::Handoff => Msg::Handoff(id),
                    GuestAction::Kick => Msg::Kick(id),
                },
            );
        }
        Msg::Handoff(id) => {
            if app.host.handoff.is_none()
                && let Some(status) = guest(app, id)
            {
                app.dialog = Some(Dialog::OfferHandoff {
                    guest: id,
                    name: status.name.clone(),
                });
            }
        }
        Msg::MicVolume(percent) => {
            app.settings.audio.mic_volume = percent.min(MAX_VOICE_VOLUME);
            if app.host.room.is_some() {
                return vec![Effect::SetMicGain {
                    host: true,
                    gain: voice_gain(app.settings.audio.mic_volume),
                }];
            }
        }
        Msg::MicVolumeDone => return app.settings_changed(),
        Msg::SelfMute(muted) => {
            if let Some(room) = app.host.room.as_mut() {
                // Se ve ya; manda hasta que una lista lo confirme.
                room.voice.mic_muted = muted;
                room.mute_pending = Some(muted);
                return vec![Effect::SetMicMuted { host: true, muted }];
            }
        }
        Msg::CancelHandoff => {
            if app.host.handing_over {
                return Vec::new();
            }
            if let Some((guest, name)) = app.host.handoff.take() {
                app.toast(
                    Tone::Info,
                    t!("host-ya-no-le-pasas-la-sala", name = name.as_str()),
                );
                return vec![Effect::CancelHandoff(guest)];
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
        Msg::AllowJoin(user) => {
            if dismiss_join_request(app, user).is_some() {
                return app.update(Message::Home(super::home::Msg::Invite(user)));
            }
        }
        Msg::DeclineJoin(user) => {
            if dismiss_join_request(app, user).is_some() && app.host.room.is_some() {
                return vec![Effect::DeclineJoin(user)];
            }
        }
        Msg::ChangeSource(open) => {
            if app.host.room.is_some() && app.host.changing_source != open {
                cue(if open { Cue::Open } else { Cue::Close });
            }
            return change_source(app, open);
        }
        Msg::ApplySource => return apply_source(app),
        Msg::Expand(person) => {
            let (motion, now) = (app.motion(), app.now);
            let fold = app.host.motion.people.entry(person).or_default();
            let open = !fold.is_open();
            fold.set(open, motion, now);
            cue(if open { Cue::Open } else { Cue::Close });
        }
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
    if mode.source() != source {
        host.target = None;
        host.picked = None;
        return refresh(app);
    }
    Vec::new()
}

/// Anfitrión: confirmado pasar la sala a `guest`: se le ofrece y se espera su respuesta
/// ([`SessionEvent::HandoffReply`]).
pub fn offer_handoff(app: &mut App, guest: GuestId, name: String) -> Vec<Effect> {
    if app.host.room.is_none() || app.host.handoff.is_some() {
        return Vec::new();
    }
    // Mientras no conteste, la sala lo dice con un aviso que deja retirar la oferta.
    app.host.handoff = Some((guest, name));
    app.host.handing_over = false;
    vec![Effect::OfferHandoff(guest)]
}

/// Invitado: aceptada la sala que pasa el anfitrión: se abre una sala solo de voz y chat (desde
/// ella se puede compartir) y, cuando tenga código, se le contesta ([`SessionEvent::RoomCreated`]).
pub fn accept_handoff(app: &mut App) -> Vec<Effect> {
    if app.host.is_open() || app.guest.is_none() {
        return vec![Effect::ReplyHandoff(None)];
    }
    let host = &mut app.host;
    host.mode = ShareMode::Nothing;
    host.target = None;
    host.picked = None;
    host.takeover = Takeover::Opening;
    start(app)
}

/// Abre la sala con lo elegido.
fn start(app: &mut App) -> Vec<Effect> {
    if app.host.is_open() {
        return Vec::new();
    }
    let audio = app.host.game_capture(&app.settings.audio.excluded_apps);
    let guests_muted = app.settings.room.guests_muted;
    let voice = app.settings.audio.host_voice;
    let now = app.now;
    let host = &mut app.host;
    host.starting = true;
    host.starting_since = Some(now);
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
    if app.host.room.is_none() {
        return Vec::new();
    }
    set_changing(app, open);
    let host = &mut app.host;
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

/// Despliega o pliega «Cambiar fuente», con su altura.
fn set_changing(app: &mut App, open: bool) {
    let (motion, now) = (app.motion(), app.now);
    app.host.changing_source = open;
    app.host.motion.source.set(open, motion, now);
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
    let mut effects = Vec::new();
    // Sin nada que compartir no hay monitores, ventanas ni aplicaciones que mirar.
    if app.host.mode != ShareMode::Nothing {
        app.host.refreshing = true;
        effects.push(Effect::LoadShareOptions(app.host.source()));
    }
    if matches!(app.friends, Load::Idle) && app.account.is_some() {
        app.friends = Load::Loading;
        effects.push(Effect::LoadFriends);
    }
    effects
}

/// Llegó la lista de amigos o un cambio de presencia (para «en vivo» y su «hace N s»).
pub fn on_friends(app: &mut App) {
    app.host.friends_at = Some(app.now);
}

/// Un amigo pide entrar: con la sala abierta aquí, su tarjeta (una por amigo: la nueva sustituye
/// a la anterior) y un aviso si Compartir no está a la vista; sin sala en este equipo (la tiene
/// abierta otro dispositivo de la cuenta), solo el aviso.
pub fn on_join_requested(app: &mut App, user: u64, name: String) {
    let visible = app.screen == Screen::Main && app.page == Page::Host;
    let room_here = app.host.room.is_some();
    if !visible || !room_here {
        app.toast(
            Tone::Info,
            t!("host-name-pide-entrar-en-tu-sala", name = name),
        );
    }
    if !room_here {
        return;
    }
    take_join_request(app, user);
    app.host.join_requests.push(JoinRequest {
        user,
        name,
        at: app.now,
    });
}

/// Quita (y devuelve) la petición de entrar de `user`.
fn take_join_request(app: &mut App, user: u64) -> Option<JoinRequest> {
    let requests = &mut app.host.join_requests;
    let at = requests.iter().position(|request| request.user == user)?;
    Some(requests.remove(at))
}

/// Quita la petición de entrar de `user` (contestada, o porque entró): su tarjeta se pliega.
fn dismiss_join_request(app: &mut App, user: u64) -> Option<JoinRequest> {
    let request = take_join_request(app, user)?;
    leave_request(app, request.clone(), app.now);
    Some(request)
}

/// La tarjeta de `request` empieza a plegarse en `since` (sin animaciones, se va sin más).
fn leave_request(app: &mut App, request: JoinRequest, since: Instant) {
    if app.motion().exit_duration(Token::Panel).is_zero() {
        return;
    }
    app.host
        .motion
        .requests_leaving
        .push(LeavingRequest { request, since });
}

/// Pasa el tiempo: las peticiones de entrar sin respuesta caducan (y se pliegan), y lo que
/// terminó de irse se olvida.
pub fn tick(app: &mut App) {
    let now = app.now;
    let (expired, kept): (Vec<JoinRequest>, Vec<JoinRequest>) =
        std::mem::take(&mut app.host.join_requests)
            .into_iter()
            .partition(|request| now.saturating_duration_since(request.at) >= JOIN_REQUEST_TTL);
    app.host.join_requests = kept;
    for request in expired {
        let since = (request.at + JOIN_REQUEST_TTL).min(now);
        leave_request(app, request, since);
    }
    let exit = app.motion().exit_duration(Token::Panel);
    let enter = app.motion().duration(Token::Panel);
    let motion = &mut app.host.motion;
    motion
        .requests_leaving
        .retain(|leaving| now.saturating_duration_since(leaving.since) < exit);
    motion
        .leaving
        .retain(|leaving| now.saturating_duration_since(leaving.since) < exit);
    motion
        .arrived
        .retain(|_, at| now.saturating_duration_since(*at) < enter);
    if motion
        .copied_at
        .is_some_and(|at| now.saturating_duration_since(at) >= COPIED)
    {
        motion.copied_at = None;
    }
    // Los desplegables de quien ya no está, una vez cerrados del todo.
    let present: Vec<GuestId> = app
        .host
        .room
        .as_ref()
        .map(|room| room.guests.iter().map(|status| status.guest).collect())
        .unwrap_or_default();
    app.host.motion.people.retain(|person, fold| match person {
        Person::Me => true,
        Person::Guest(id) => present.contains(id) || fold.animating(now),
    });
}

/// Si la sala está delante.
fn room_visible(app: &App) -> bool {
    app.screen == Screen::Main && app.page == Page::Host && app.host.room.is_some()
}

/// Cuándo hace falta el próximo `Tick`: al caducar la petición de entrar más antigua y, con la
/// sala a la vista, cada segundo para su cuenta atrás y para el tiempo «EN VIVO»; al acabar de
/// plegarse lo que se va y al volver el icono de copiar.
pub fn next_tick(app: &App) -> Option<Instant> {
    let now = app.now;
    let soon = now + Duration::from_millis(1);
    let visible = app.screen == Screen::Main && app.page == Page::Host;
    let mut next: Option<Instant> = None;
    let mut consider = |at: Instant| {
        next = Some(next.map_or(at, |next| next.min(at)));
    };
    let second = |since: Instant| {
        let elapsed = now.saturating_duration_since(since).as_secs();
        since + Duration::from_secs(elapsed + 1)
    };
    if let Some(oldest) = app
        .host
        .join_requests
        .iter()
        .map(|request| request.at)
        .min()
    {
        consider((oldest + JOIN_REQUEST_TTL).max(soon));
        if visible {
            consider(second(oldest));
        }
    }
    let exit = app.motion().exit_duration(Token::Panel);
    let motion = &app.host.motion;
    for leaving in &motion.requests_leaving {
        consider((leaving.since + exit).max(soon));
    }
    for leaving in &motion.leaving {
        consider((leaving.since + exit).max(soon));
    }
    if let Some(at) = motion.copied_at {
        consider((at + COPIED).max(soon));
    }
    if room_visible(app)
        && let Some(since) = motion.live_since
    {
        consider(second(since));
    }
    next
}

/// Una curva de `token` empezada en `since` (0 a 1); sin animaciones, ya terminada.
fn progress(app: &App, since: Instant, token: Token, delay: Duration) -> f32 {
    let duration = app.motion().duration(token);
    let start = since + delay;
    if duration.is_zero() || app.now < since {
        return if app.now >= start { 1.0 } else { 0.0 };
    }
    let t = (app.now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0);
    if t >= 1.0 {
        1.0
    } else {
        token.easing().value(t)
    }
}

/// Lo que lleva de su salida algo que empezó a irse en `since` (0 a 1).
fn exit_progress(app: &App, since: Instant) -> f32 {
    let duration = app.motion().exit_duration(Token::Panel);
    if duration.is_zero() {
        return 1.0;
    }
    let t = (app.now.saturating_duration_since(since).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0);
    if t >= 1.0 { 1.0 } else { EXIT_EASING.value(t) }
}

/// Si algo de la sala se mueve (hace falta otro cuadro), con la sala delante. Con la sala en
/// reposo, nada: la transmisión no anima nada de continuo.
pub fn animating(app: &App) -> bool {
    let visible = app.screen == Screen::Main && app.page == Page::Host;
    if !visible || (app.host.room.is_none() && app.host.motion.preset.is_none()) {
        return false;
    }
    let now = app.now;
    let motion = &app.host.motion;
    let moment = app.motion().duration(Token::Moment);
    let panel = app.motion().duration(Token::Panel);
    let exit = app.motion().exit_duration(Token::Panel);
    let within = |at: Option<Instant>, duration: Duration| {
        at.is_some_and(|at| now.saturating_duration_since(at) < duration)
    };
    within(motion.opened_at, moment)
        || within(motion.code_at, moment)
        || within(motion.glow_at, moment)
        || within(motion.live_ended, app.motion().duration(Token::Screen))
        || within(motion.copied_at, panel)
        || within(motion.preset.map(|(_, at)| at), panel)
        || motion.source.animating(now)
        || motion.people.values().any(|fold| fold.animating(now))
        || motion.arrived.values().any(|at| within(Some(*at), panel))
        || motion.leaving.iter().any(|l| within(Some(l.since), exit))
        || motion
            .requests_leaving
            .iter()
            .any(|l| within(Some(l.since), exit))
        || app.host.join_requests.iter().any(|request| {
            within(Some(request.at), panel)
                && !motion.settled.is_some_and(|settled| request.at <= settled)
        })
}

/// Deja la sala sin nada a medio animar (las capturas y las pruebas de que nada se mueve): lo que
/// entraba ya entró, lo que se iba ya se fue y cada desplegable queda abierto o cerrado del todo.
/// El tiempo «EN VIVO» no se toca.
pub fn settle(app: &mut App) {
    let now = app.now;
    let motion = &mut app.host.motion;
    motion.settled = Some(now);
    motion.opened_at = None;
    motion.code_at = None;
    motion.copied_at = None;
    motion.glow_at = None;
    motion.live_ended = None;
    motion.preset = None;
    motion.arrived.clear();
    motion.leaving.clear();
    motion.requests_leaving.clear();
    motion.source = motion.source.at_rest();
    for fold in motion.people.values_mut() {
        *fold = fold.at_rest();
    }
}

/// Lo que se transmite pasó a ser `sharing` (o nada): arranca el tiempo «EN VIVO» con su brillo,
/// o el rojo se apaga.
fn on_shared(app: &mut App, sharing: Option<&Sharing>) {
    let now = app.now;
    let live = sharing.is_some_and(|sharing| sharing.mode != ShareMode::Nothing);
    let motion = &mut app.host.motion;
    match (live, motion.live_since.is_some()) {
        (true, false) => {
            motion.live_since = Some(now);
            motion.glow_at = Some(now);
            motion.live_ended = None;
        }
        (false, true) => {
            motion.live_since = None;
            motion.glow_at = None;
            motion.live_ended = Some(now);
        }
        _ => {}
    }
}

/// Lo que lleva la transmisión en vivo: «EN VIVO 12:04» (`None` si no se comparte nada).
pub fn live_label(app: &App) -> Option<String> {
    let since = app.host.motion.live_since?;
    app.host.room.as_ref()?;
    let seconds = app.now.saturating_duration_since(since).as_secs();
    Some(format!(
        "{} {}",
        t!("host-en-vivo"),
        stats::elapsed(seconds)
    ))
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
            // El token del selector vale para abrir la sala sin volver a preguntar, también en
            // la próxima ejecución.
            let mut effects = match &picked.restore_token {
                Some(token) => on_restore_token(app, source.kind(), Some(token.clone())),
                None => Vec::new(),
            };
            app.host.picked = Some(picked);
            if app.host.room.is_some() {
                effects.extend(apply_source(app));
            }
            return effects;
        }
        Ok(None) => {}
        Err(err) => app.toast(
            Tone::Warning,
            t!("host-no-se-pudo-abrir-el-selector", error = err),
        ),
    }
    Vec::new()
}

/// El portal de captura dio un token de restauración para `kind` (al elegir en su selector o al
/// arrancar la captura, que lo renueva) o rechazó el guardado (`None`): el estado y `portal.json`
/// se ponen al día. Con el token guardado, el diálogo del sistema no vuelve a salir; sin él, sale
/// una vez en la próxima sala y lo que conteste se guarda.
pub fn on_restore_token(app: &mut App, kind: ShareKind, token: Option<String>) -> Vec<Effect> {
    if app.host.restore_tokens.get(kind) == token.as_deref() {
        return Vec::new();
    }
    app.host.restore_tokens.set(kind, token);
    vec![Effect::SaveRestoreTokens(app.host.restore_tokens.clone())]
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
                volumes: BTreeMap::new(),
                guests_muted,
                // Hasta la primera lista: la voz pedida al abrir, sin «sin micrófono» de paso.
                voice: HostVoice {
                    enabled: app.settings.audio.host_voice,
                    ..HostVoice::default()
                },
                mute_pending: None,
            });
            app.page = Page::Host;
            // El momento de abrir la sala: la vista previa crece hasta su sitio y, si se comparte
            // algo, arranca «EN VIVO».
            app.host.motion = RoomMotion {
                opened_at: Some(app.now),
                ..RoomMotion::default()
            };
            let shared = app.host.shared.clone();
            on_shared(app, shared.as_ref());
            // La sala lleva su propia señalización: la de espera ya sobra.
            let mut effects = vec![Effect::StopLobby];
            match app.host.takeover {
                Takeover::Withdrawn => {
                    // El anfitrión retiró la oferta mientras se abría: se cierra y se sigue en
                    // la suya (`Withdrawn` se queda hasta `HostEnded`, que cierra sin avisos).
                    app.host.stopping = true;
                    effects.push(Effect::StopHost);
                    return effects;
                }
                Takeover::Opening => {
                    // Quien se calló en la sala de antes sigue callado en la suya.
                    let muted = app
                        .guest
                        .as_ref()
                        .and_then(|guest| guest.stats.as_ref())
                        .is_some_and(|stats| stats.mic_muted);
                    if muted {
                        if let Some(room) = app.host.room.as_mut() {
                            room.voice.mic_muted = true;
                            room.mute_pending = Some(true);
                        }
                        effects.push(Effect::SetMicMuted {
                            host: true,
                            muted: true,
                        });
                    }
                }
                Takeover::None | Takeover::Replied(_) => {}
            }
            // Los micrófonos y salidas, para cambiarlos con la sala abierta.
            effects.extend(settings::request_audio(app));
            // La voz propia sale al volumen normal: se le aplica el recordado.
            if app.settings.audio.mic_volume != DEFAULT_VOICE_VOLUME {
                effects.push(Effect::SetMicGain {
                    host: true,
                    gain: voice_gain(app.settings.audio.mic_volume),
                });
            }
            effects.extend(app.preview_visibility());
            return effects;
        }
        Event::HostStarted(Err(err)) => {
            app.host.starting = false;
            app.host.shared = None;
            let takeover = std::mem::take(&mut app.host.takeover);
            if takeover == Takeover::Withdrawn {
                // Ya no hacía falta: se sigue en la sala del anfitrión.
                return Vec::new();
            }
            if takeover == Takeover::Opening {
                // No se pudo abrir la sala que se aceptó: se le dice que no al anfitrión y se
                // sigue de invitado en la suya.
                app.toast(
                    Tone::Warning,
                    t!("host-no-se-pudo-abrir-la-sala", error = err.as_str()),
                );
                return vec![Effect::ReplyHandoff(None)];
            }
            app.host.error = Some(err);
            return vec![Effect::StartLobby];
        }
        Event::HostGuests(guests, mut voice) => {
            let mut effects = Vec::new();
            if let Some(room) = app.host.room.as_mut() {
                // La lista pudo tomarse antes del último clic en el altavoz propio: hasta que
                // traiga lo pedido, manda lo pedido.
                if let Some(muted) = room.mute_pending {
                    if voice.mic_muted == muted {
                        room.mute_pending = None;
                    } else {
                        voice.mic_muted = muted;
                    }
                }
                room.voice = voice;
                // A quien llega se le aplica el volumen recordado por su nombre (una vez: lo que
                // se toque después manda); quien se fue no deja entrada.
                room.volumes
                    .retain(|id, _| guests.iter().any(|status| status.guest == *id));
                room.muted
                    .retain(|id| guests.iter().any(|status| status.guest == *id));
                // El silencio recordado por su nombre (se guarda al tocarlo) vale siempre: también
                // si vuelve a entrar con otro id.
                for status in &guests {
                    let muted = app.settings.audio.voice_muted(&status.name);
                    if muted != room.muted.contains(&status.guest) {
                        if muted {
                            room.muted.insert(status.guest);
                        } else {
                            room.muted.remove(&status.guest);
                        }
                        effects.push(Effect::SetVoiceMuted {
                            host: true,
                            source: status.guest.0,
                            muted,
                        });
                    }
                }
                for status in &guests {
                    if room.volumes.contains_key(&status.guest) {
                        continue;
                    }
                    let percent = app.settings.audio.voice_volume(&status.name);
                    if percent != DEFAULT_VOICE_VOLUME {
                        room.volumes.insert(status.guest, percent);
                        effects.push(Effect::SetVoiceGain {
                            host: true,
                            source: status.guest.0,
                            gain: voice_gain(percent),
                        });
                    }
                }
                // Quien llega entra deslizándose; quien ya no está se pliega donde estaba.
                let now = app.now;
                let motion = &mut app.host.motion;
                for status in &guests {
                    if !room.guests.iter().any(|old| old.guest == status.guest) {
                        motion.arrived.entry(status.guest).or_insert(now);
                    }
                }
                for (index, old) in room.guests.iter().enumerate() {
                    let gone = !guests.iter().any(|status| status.guest == old.guest);
                    let known = motion.leaving.iter().any(|l| l.status.guest == old.guest);
                    if gone && !known {
                        motion.leaving.push(LeavingGuest {
                            status: old.clone(),
                            index,
                            since: now,
                        });
                    }
                }
                room.guests = guests;
            }
            return effects;
        }
        Event::PreviewUnavailable => {
            if let Some(room) = app.host.room.as_mut() {
                room.preview_unavailable = true;
            }
        }
        Event::HostEnded(reason) => {
            let was_open = app.host.room.take().is_some();
            // La sala que se abría para recibir la de otro y que este retiró: ya se avisó al
            // retirarla, y se sigue de invitado en la suya (sin sala de espera).
            if std::mem::take(&mut app.host.takeover) == Takeover::Withdrawn {
                let host = &mut app.host;
                host.join_requests.clear();
                host.shared = None;
                host.switching = None;
                host.starting = false;
                host.stopping = false;
                host.changing_source = false;
                host.motion = RoomMotion::default();
                if app.page == Page::Host {
                    app.page = Page::Home;
                }
                return if app.guest.is_none() && app.account.is_some() {
                    vec![Effect::StartLobby]
                } else {
                    Vec::new()
                };
            }
            let host = &mut app.host;
            host.join_requests.clear();
            host.shared = None;
            host.switching = None;
            host.starting = false;
            host.stopping = false;
            host.changing_source = false;
            host.motion = RoomMotion::default();
            if let Some(reason) = reason {
                app.toast(
                    Tone::Error,
                    t!("host-la-sala-se-cerro-reason", reason = reason),
                );
            } else if was_open {
                app.toast(Tone::Info, t!("host-has-dejado-de-compartir"));
            }
            app.host.handoff = None;
            app.host.handing_over = false;
            if let Some(code) = app.host.moving_to.take() {
                // La sala pasó a otro: se entra en la suya como invitado.
                return super::guest::start(app, code);
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
        Event::HandedOver(result) => match result {
            Ok(code) if app.host.room.is_some() => {
                // El silencio propio de esta sala sigue en la nueva.
                app.carried_mute = app.host.room.as_ref().map(|room| room.voice.mic_muted);
                app.host.moving_to = Some(code);
                app.host.stopping = true;
                return vec![Effect::StopHost];
            }
            Ok(_) => {}
            Err(err) => {
                app.host.handoff = None;
                app.host.handing_over = false;
                app.toast(
                    Tone::Warning,
                    t!("host-no-se-pudo-pasar-la-sala", error = err),
                );
            }
        },

        Event::Host(event) => return session_event(app, event),
        Event::VideoChanged(result) => {
            if app.host.room.is_some() {
                match result {
                    Ok(()) => app.toast(
                        Tone::Info,
                        t!(
                            "host-video-de-la-sala-cambiado",
                            valor = encoding_phrase(&app.settings.video)
                        ),
                    ),
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
                    let stopped = requested
                        .as_ref()
                        .is_some_and(|sharing| sharing.mode == ShareMode::Nothing);
                    app.toast(
                        Tone::Info,
                        if stopped {
                            t!("host-ya-no-compartes-nada")
                        } else {
                            t!("host-ahora-compartes-otra-cosa")
                        },
                    );
                    if requested.is_some() {
                        on_shared(app, requested.as_ref());
                        app.host.shared = requested;
                    }
                    set_changing(app, false);
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
                room.code = Some(code.clone());
                room.fingerprint = fingerprint;
                // El código cae letra a letra.
                app.host.motion.code_at = Some(app.now);
            }
            effects.extend(invite_marked(app));
            if app.host.takeover == Takeover::Opening {
                // La sala que pasó el anfitrión ya tiene código: se le contesta y se sigue en la
                // suya hasta que lo confirme (si se saliese ya, la respuesta podría no salir).
                app.host.takeover = Takeover::Replied(code.clone());
                effects.push(Effect::ReplyHandoff(Some(code)));
            }
        }
        SessionEvent::HandoffReply { guest, code } => {
            let pending = app
                .host
                .handoff
                .clone()
                .filter(|(offered, _)| *offered == guest && !app.host.handing_over);
            let Some((_, name)) = pending else {
                // Una respuesta que ya no se espera (se retiró la oferta, o no es de quien la
                // tiene): si abrió su sala, que la cierre.
                if code.is_some() && !app.host.handing_over {
                    effects.push(Effect::CancelHandoff(guest));
                }
                return effects;
            };
            match code {
                Some(code) => {
                    app.host.handing_over = true;
                    app.toast(
                        Tone::Info,
                        t!("host-pasando-la-sala-a", name = name.as_str()),
                    );
                    effects.push(Effect::HandOver {
                        code,
                        host_name: name,
                    });
                }
                None => {
                    app.host.handoff = None;
                    app.toast(
                        Tone::Info,
                        t!("host-name-no-acepto-la-sala", name = name.as_str()),
                    );
                }
            }
        }
        SessionEvent::GuestJoined {
            user,
            name,
            fingerprint,
            safety_number,
            permissions,
            ..
        } => {
            // Si había pedido entrar, ya está dentro: su tarjeta se pliega.
            dismiss_join_request(app, user);
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
            let now = app.now;
            let Some(room) = app.host.room.as_mut() else {
                return effects;
            };
            // Su fila se pliega donde estaba.
            if let Some(index) = room.guests.iter().position(|status| status.guest == guest) {
                app.host.motion.leaving.push(LeavingGuest {
                    status: room.guests[index].clone(),
                    index,
                    since: now,
                });
            }
            let name = room
                .guests
                .iter()
                .find(|status| status.guest == guest)
                .map_or_else(
                    || t!("host-el-invitado", valor = guest.0),
                    |s| s.name.clone(),
                );
            room.guests.retain(|status| status.guest != guest);
            // Su silencio y su volumen dejan libre su sitio en el audio (lo recordado por su
            // nombre se vuelve a aplicar si vuelve).
            if room.muted.remove(&guest) {
                effects.push(Effect::SetVoiceMuted {
                    host: true,
                    source: guest.0,
                    muted: false,
                });
            }
            if room.volumes.remove(&guest).is_some() {
                effects.push(Effect::SetVoiceGain {
                    host: true,
                    source: guest.0,
                    gain: voice_gain(DEFAULT_VOICE_VOLUME),
                });
            }
            // Si era a quien se ofrecía la sala y aún no contestó, la oferta se acaba (al pasarla,
            // en cambio, sale porque ya va a la suya).
            if !app.host.handing_over
                && app
                    .host
                    .handoff
                    .as_ref()
                    .is_some_and(|(offered, _)| *offered == guest)
            {
                app.host.handoff = None;
            }
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
        SessionEvent::Audio(issue) => app.toast(Tone::Warning, settings::audio_issue_text(&issue)),
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
//
// Los mismos en Compartir y en Ajustes → Vídeo (de aquí los toma `settings`): un solo texto para
// cada perfil, resolución, códec, codificador y para las explicaciones de 4:4:4 y HDR.

/// Resolución máxima, corta («1080p», «Nativa»: la de la fuente hasta 4K).
fn resolution_short(resolution: Resolution) -> String {
    match resolution {
        Resolution::P2160 => t!("host-nativa").to_owned(),
        other => format!("{}p", other.size().1),
    }
}

/// Resolución máxima en el desplegable.
pub fn resolution_label(resolution: Resolution) -> String {
    match resolution {
        Resolution::P2160 => t!("host-nativa-hasta-4k").to_owned(),
        other => {
            let (width, height) = other.size();
            format!("{height}p ({width} × {height})")
        }
    }
}

/// Lo que reabre el codificador con la sala abierta: «1080p · 60 fps · AV1».
fn encoding_phrase(video: &Video) -> String {
    let codec = codec_choices()
        .into_iter()
        .find(|c| c.value == video.codec)
        .map_or_else(|| t!("host-automatico").to_owned(), |c| c.label);
    format!(
        "{} · {} · {codec}",
        resolution_short(video.max_resolution),
        t!("host-fps", fps = video.fps)
    )
}

/// Lo que fija un perfil: «1080p · 60 fps».
fn video_spec(video: &Video) -> String {
    let mut parts = vec![
        resolution_short(video.max_resolution),
        t!("host-fps", fps = video.fps),
    ];
    if video.chroma_444 {
        parts.push("4:4:4".to_owned());
    }
    if video.hdr {
        parts.push("HDR".to_owned());
    }
    parts.join(" · ")
}

/// Nombre de un perfil de calidad.
pub fn preset_name(preset: QualityPreset) -> &'static str {
    match preset {
        QualityPreset::MinLatency => t!("host-perfil-min-latencia"),
        QualityPreset::Balanced => t!("host-perfil-equilibrado"),
        QualityPreset::MaxQuality => t!("host-perfil-max-calidad"),
        QualityPreset::Custom => t!("host-perfil-personalizado"),
    }
}

/// Nombre corto de un perfil (en los segmentados).
pub fn preset_short(preset: QualityPreset) -> &'static str {
    match preset {
        QualityPreset::MinLatency => t!("host-perfil-min-latencia-corto"),
        QualityPreset::Balanced => t!("host-perfil-equilibrado"),
        QualityPreset::MaxQuality => t!("host-perfil-max-calidad-corto"),
        QualityPreset::Custom => t!("host-perfil-personalizado"),
    }
}

/// Para qué sirve un perfil.
pub fn preset_help(preset: QualityPreset) -> &'static str {
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

fn option_name(option: VideoOption) -> &'static str {
    match option {
        VideoOption::Resolution => t!("host-opcion-resolucion"),
        VideoOption::Fps => t!("host-opcion-fps"),
        VideoOption::Codec => t!("host-opcion-codec"),
        VideoOption::Chroma444 => "4:4:4",
        VideoOption::Hdr => "HDR",
    }
}

/// Cuándo se aplica cada opción, agrupado («Al momento: códec, resolución y fps · En la próxima
/// sala: …»), sacado de [`Video::when`] con las etiquetas de Ajustes ([`Applies`]).
fn when_summary() -> String {
    const ALL: [VideoOption; 5] = [
        VideoOption::Codec,
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
                    when = applies(when).label(),
                    valor = join_names(&names)
                )
            })
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Cuándo surte efecto lo que se aplica en `when`, con el nombre de Ajustes.
fn applies(when: When) -> Applies {
    match when {
        When::Live => Applies::Now,
        When::NextRoom => Applies::NextRoom,
    }
}

/// Los códecs que se pueden elegir.
pub fn codec_choices() -> Vec<Pick<CodecChoice>> {
    vec![
        pick(CodecChoice::Auto, t!("host-automatico")),
        pick(CodecChoice::Av1, "AV1"),
        pick(CodecChoice::Hevc, "HEVC"),
        pick(CodecChoice::H264, "H.264"),
    ]
}

/// Codificadores de esta plataforma.
pub fn encoder_choices() -> Vec<Pick<EncoderChoice>> {
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

/// El códec de una elección (`None`: automático).
pub fn codec_of(choice: CodecChoice) -> Option<coroom_session::codec::Codec> {
    use coroom_session::codec::Codec;
    match choice {
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

/// Explicación de 4:4:4 con lo que hace este equipo y el códec elegido.
pub fn chroma_hint(app: &App) -> String {
    chroma_hint_for(
        app.video_support.as_ref(),
        codec_of(app.settings.video.codec),
    )
}

/// Explicación de 4:4:4 con lo que hace este equipo (`support`, `None` mientras se comprueba) y
/// el códec `codec` (`None`: automático). Ajustes la pide para cada códec, para reservar el alto
/// de la más larga.
pub fn chroma_hint_for(
    support: Option<&crate::ui::app::VideoSupport>,
    codec: Option<coroom_session::codec::Codec>,
) -> String {
    let lead = t!("host-croma-lead");
    let Some(support) = support else {
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
    match codec {
        Some(codec) if !support.yuv444.iter().any(|(c, _)| *c == codec) => t!(
            "host-croma-no-con-este-codec",
            lead = lead,
            codec = codec,
            valor = with
        ),
        _ => t!("host-croma-disponible-con", lead = lead, valor = with),
    }
}

/// Explicación de HDR con lo que hace este equipo y el códec elegido.
pub fn hdr_hint(app: &App) -> String {
    hdr_hint_for(
        app.video_support.as_ref(),
        codec_of(app.settings.video.codec),
    )
}

/// Explicación de HDR con lo que hace este equipo y el códec `codec` (como [`chroma_hint_for`]).
pub fn hdr_hint_for(
    support: Option<&crate::ui::app::VideoSupport>,
    codec: Option<coroom_session::codec::Codec>,
) -> String {
    let Some(support) = support else {
        return t!("host-hdr-comprobando").to_owned();
    };
    let share = if support.hdr10.is_empty() {
        t!("host-hdr-no-disponible").to_owned()
    } else {
        let with = join_codecs(support.hdr10.iter().map(|(_, label)| label.clone()));
        match codec {
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

/// La columna «Al momento» (verde) o «En la próxima sala» (gris) de una opción: la misma que en
/// Ajustes, del ancho de la más ancha.
fn tag<'a>(when: When) -> Element<'a, Message> {
    settings::when_column(applies(when))
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
    coroom_ui::release::release_to_pick(
        iced_widget::pick_list(options, selected, on_select)
            .padding(Padding::from([8.0, 12.0]))
            .text_size(type_size::BODY)
            .font(coroom_ui::font::UI)
            .width(Length::Fill),
    )
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

/// Cómo se nombra lo elegido en el selector del sistema (Wayland solo da el tamaño).
fn picked_label(target: &CaptureTarget) -> String {
    match target {
        CaptureTarget::Monitor { name, .. } if name.is_empty() => t!("host-monitor").to_owned(),
        CaptureTarget::Monitor { name, .. } => t!("host-monitor-de-size", size = name.as_str()),
        CaptureTarget::Window { title, .. } => t!("host-ventana-de-size", size = title.as_str()),
    }
}

/// Cómo se nombra una transmisión («Monitor principal · 2560 × 1440», «Juego»…).
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
        (ShareMode::Nothing, _) => t!("host-solo-voz-y-chat").to_owned(),
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
    if state.mode == ShareMode::Nothing {
        return ui::body(t!("host-solo-voz-detalle"))
            .wrapping(Wrapping::WordOrGlyph)
            .into();
    }
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

/// Segmentado Pantalla / Ventana.
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
        placeholder(
            state.mode.icon(),
            if state.mode == ShareMode::Nothing {
                t!("host-solo-voz-y-chat")
            } else {
                t!("host-vista-previa-al-abrir")
            },
        ),
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
    // Algo elegido (aún no en vivo: la sala no está abierta) lleva el borde de acento.
    let class = if state.mode == ShareMode::Nothing {
        ContainerClass::Card
    } else {
        ContainerClass::CardAccent
    };
    container(card)
        .padding(Padding::from([space::S, space::M]))
        .width(Length::Fill)
        .height(if real { Length::Fill } else { Length::Shrink })
        .class(class)
        .into()
}

/// «● en vivo», con cuándo llegó la última lista de amigos o el último cambio de presencia al
/// pasar el ratón: «hace N s» cambiaba de ancho cada segundo.
fn friends_live<'a>(app: &App) -> Option<Element<'a, Message>> {
    let at = app.host.friends_at?;
    let secs = app.now.saturating_duration_since(at).as_secs();
    let detail = if secs < 60 {
        t!("host-actualizado-hace-segundos", n = secs)
    } else {
        t!("host-actualizado-hace-minutos", n = secs / 60)
    };
    Some(ui::with_tooltip(
        row![
            ui::dot(Tone::Success),
            ui::caption(t!("host-en-vivo-corto"))
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
        detail,
    ))
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
    if let Some(label) = friends_live(app)
        && app.friends.loaded().is_some()
    {
        header = header.push(label);
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
            for friend in super::home::sorted_friends(friends) {
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
                // La misma insignia que en Inicio: disponible, en su sala o jugando.
                let badge = super::home::activity_badge(friend);
                list = push(
                    list,
                    container(
                        row![
                            mark,
                            avatars::view(app, &friend.display_name, friend.avatar, 28.0),
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

/// Cuánto está «encendida» la tarjeta de `preset` (0 a 1): la elegida sube y se le enciende el
/// borde en un panel; la que deja de serlo se apaga a la vez.
fn preset_lit(app: &App, preset: QualityPreset) -> f32 {
    let selected = app.settings.video.preset == preset;
    let Some((before, at)) = app.host.motion.preset else {
        return if selected { 1.0 } else { 0.0 };
    };
    let shown = progress(app, at, Token::Panel, Duration::ZERO);
    if selected {
        shown
    } else if before == preset {
        1.0 - shown
    } else {
        0.0
    }
}

/// Tarjeta grande de un perfil de calidad (`real`: la de verdad o la copia que da la altura).
fn preset_card(app: &App, preset: QualityPreset, real: bool) -> Element<'_, Message> {
    let video = &app.settings.video;
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
    let lit = preset_lit(app, preset);
    let card = button(content)
        .padding(14)
        .width(Length::Fill)
        .height(if real { Length::Fill } else { Length::Shrink })
        .class(ButtonClass::ChoiceLit { lit })
        .on_press(message);
    // Sube un poco al encenderse: se dibuja desplazada, sin mover a las demás.
    Shift::new(card, Vector::new(0.0, -app.motion().slide(LIFT) * lit)).into()
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
                .map(|preset| preset_card(app, *preset, real))
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
    if state.mode == ShareMode::Nothing {
        return t!("host-solo-tu-voz").to_owned();
    }
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
        t!("host-ninguna-aplicacion-suena-ahora").to_owned()
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
            let subtitle = t!("host-las-aplicaciones-que-empiecen-a-sonar");
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

/// Micrófono y salida de este equipo con la sala abierta: los mismos de Ajustes → Audio y voz,
/// con el mismo nombre y la misma columna («Al momento»).
fn devices_box(app: &App) -> Element<'_, Message> {
    let audio = &app.settings.audio;
    let devices: Vec<AudioDevice> = app
        .settings_page
        .audio_devices
        .loaded()
        .cloned()
        .unwrap_or_default();
    let mics = settings::device_choices(&devices, true, &audio.microphone);
    let mic = mics.iter().find(|c| c.value == audio.microphone).cloned();
    let speakers = settings::device_choices(&devices, false, &audio.speaker);
    let speaker = speakers.iter().find(|c| c.value == audio.speaker).cloned();
    let narrow = compact(app);
    let help = |content: &'static str| -> Element<'_, Message> {
        ui::secondary(content)
            .wrapping(Wrapping::WordOrGlyph)
            .into()
    };
    // En la sala estrecha, «Al momento» va bajo la descripción y el selector es más estrecho:
    // en una línea no cabían el título, la columna y el selector.
    let device_row = |title: &'static str,
                      description: &'static str,
                      control: Element<'static, Message>|
     -> Element<'static, Message> {
        if narrow {
            ui::setting_row(
                title,
                Some(
                    column![help(description), settings::when_column(Applies::Now)]
                        .spacing(space::XS)
                        .into(),
                ),
                None,
                control,
            )
        } else {
            ui::setting_row(
                title,
                Some(help(description)),
                Some(settings::when_column(Applies::Now)),
                control,
            )
        }
    };
    let width = if narrow {
        DEVICE_SELECT_COMPACT
    } else {
        DEVICE_SELECT
    };
    ui::boxed_list(vec![
        device_row(
            t!("settings-microfono"),
            t!("settings-microfono-ayuda"),
            ui::select_fixed(
                mics,
                mic,
                |c| Message::Settings(settings::Msg::Microphone(c)),
                width,
            ),
        ),
        device_row(
            t!("settings-salida"),
            t!("settings-salida-ayuda"),
            ui::select_fixed(
                speakers,
                speaker,
                |c| Message::Settings(settings::Msg::Speaker(c)),
                width,
            ),
        ),
    ])
}

/// Ancho de los selectores de dispositivo de la sala.
const DEVICE_SELECT: f32 = 280.0;

/// Ancho de los selectores de dispositivo en la sala estrecha.
const DEVICE_SELECT_COMPACT: f32 = 190.0;

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
    let page = column![
        top,
        column![quality_head, preset_cards(app)].spacing(space::XS),
        advanced(app),
        audio_box(app, true),
    ]
    .spacing(space::M);
    // El error va con los botones, en una ranura que está siempre (con el espacio de debajo
    // dentro): los bloques de la página no cambian de posición cuando aparece.
    let error = state.error.as_ref().map(|error| {
        container(ui::banner(
            Tone::Error,
            t!("host-no-se-pudo-abrir-la-sala", error = error),
            Some((t!("host-reintentar"), on(Msg::Start))),
        ))
        .padding(Padding {
            bottom: space::M,
            ..Padding::ZERO
        })
        .into()
    });
    let actions: Element<'_, Message> = if state.starting {
        let since = state.starting_since.unwrap_or(app.now);
        // Sin el sondeo de códecs (primer arranque o drivers nuevos) la sala lo espera: se dice.
        let label = if coroom_session::codec::Capabilities::is_probed() {
            t!("host-abriendo-la-sala")
        } else {
            t!("host-comprobando-los-codecs")
        };
        container(ui::progress(
            label,
            None,
            app.now.duration_since(since).as_secs_f32() / 1.4,
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
    let page = page.push(column![
        ui::slot(error),
        row![ui::fill(), actions].align_y(Alignment::Center)
    ]);
    ui::page("share", page)
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

/// Si la sala no transmite nada (solo voz y chat) y no se está cambiando a otra cosa.
fn voice_only(state: &State) -> bool {
    state.switching.is_none()
        && state
            .shared
            .as_ref()
            .is_some_and(|shared| shared.mode == ShareMode::Nothing)
}

/// Cabecera de la sala: «Sala de Ana», el código (que cae letra a letra al crearse la sala), los
/// invitados y la ruta; a la derecha, el estado, copiar el código, Diagnóstico y «Cerrar la sala».
fn room_header<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let title = app.account.as_ref().map_or_else(
        || t!("host-tu-sala").to_owned(),
        |account| t!("host-sala-de", name = account.display_name.as_str()),
    );
    let mut facts = vec![match room.guests.len() {
        0 => t!("host-sin-invitados").to_owned(),
        1 => t!("host-un-invitado").to_owned(),
        n => t!("host-n-invitados", n = n),
    }];
    if let Some(route) = route_summary(room) {
        facts.push(route.to_owned());
    }
    let facts = facts.join(" · ");
    let code: Element<'a, Message> = match &room.code {
        Some(code) => row![
            ui::secondary(t!("host-codigo")).wrapping(Wrapping::None),
            code_letters(app, code)
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center)
        .into(),
        None => ui::secondary(t!("host-creando-la-sala"))
            .wrapping(Wrapping::None)
            .into(),
    };
    // El código con su botón de copiar al lado; los invitados y la ruta debajo, en su línea (en
    // la de la ventana estrecha no cabían junto al código).
    let texts = column![
        ui::title(title),
        row![code, copy_button(app, room)]
            .spacing(space::XS)
            .align_y(Alignment::Center),
        ui::one_line(facts, ui::secondary).width(Length::Fill),
    ]
    .spacing(2)
    .width(Length::Fill);
    let diagnostics = Message::Diagnostics(crate::ui::screens::diagnostics::Msg::Open);
    let close = ui::button_with(
        Kind::Destructive,
        t!("host-cerrar-la-sala"),
        Some(Icon::Close),
        (!app.host.stopping).then(|| on(Msg::Stop)),
    );
    if compact(app) {
        // En la sala estrecha, en dos pisos: el estado y los botones debajo, Diagnóstico solo con
        // su icono.
        let actions = row![
            room_badge(app, room, Horizontal::Left),
            ui::fill(),
            ui::icon_button(Icon::Stethoscope, t!("app-diagnostico"), Some(diagnostics)),
            close,
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center);
        return column![texts, actions].spacing(space::S).into();
    }
    let actions = row![
        room_badge(app, room, Horizontal::Right),
        ui::button_with(
            Kind::Normal,
            t!("app-diagnostico"),
            Some(Icon::Stethoscope),
            Some(diagnostics),
        ),
        close,
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    row![texts, actions]
        .spacing(space::M)
        .align_y(Alignment::Center)
        .into()
}

/// El código, letra a letra: al crearse la sala cada una cae a su sitio y aparece, escalonadas
/// ([`Token::Moment`] en total); con movimiento reducido solo aparecen.
fn code_letters<'a>(app: &App, code: &str) -> Element<'a, Message> {
    let motion = app.motion();
    let fall = motion.slide(space::S);
    let color = app.theme.palette.text;
    let at = app.host.motion.code_at;
    let letters = spaced(code)
        .chars()
        .enumerate()
        .map(|(index, letter)| {
            let shown = at.map_or(1.0, |at| {
                progress(app, at, Token::Panel, motion.stagger(index))
            });
            let glyph = ui::strong(letter.to_string())
                .wrapping(Wrapping::None)
                .class(TextClass::Custom(color.alpha(shown)));
            Shift::new(glyph, Vector::new(0.0, -fall * (1.0 - shown))).into()
        })
        .collect::<Vec<Element<'a, Message>>>();
    Row::with_children(letters).into()
}

/// La insignia de estado de la cabecera, en una ranura del ancho de la más ancha.
fn room_badge<'a>(app: &App, room: &Room, align: Horizontal) -> Element<'a, Message> {
    let badge = |variant: usize| -> Element<'a, Message> {
        match variant {
            0 => ui::status_badge(Tone::Success, t!("host-transmitiendo")),
            1 => ui::status_badge(Tone::Neutral, t!("host-modo-solo-voz")),
            2 => ui::status_badge(Tone::Neutral, t!("host-esperando-invitados")),
            _ => ui::status_badge(Tone::Warning, t!("host-eligiendo-codec")),
        }
    };
    let current = if voice_only(&app.host) {
        1
    } else {
        match (&room.codec, room.guests.is_empty()) {
            (Some(_), _) => 0,
            (None, true) => 2,
            (None, false) => 3,
        }
    };
    ui::reserve((0..4).map(badge), Some(badge(current)))
        .align_x(align)
        .into()
}

/// Lado del botón de copiar, junto al código (más pequeño que un botón suelto: va en una línea
/// de texto).
const COPY_SIZE: f32 = 28.0;

/// «Copiar el código»: al copiar, el icono se vuelve un tilde con un pulso y vuelve a los
/// [`COPIED`].
fn copy_button<'a>(app: &App, room: &Room) -> Element<'a, Message> {
    let copied_at = app
        .host
        .motion
        .copied_at
        .filter(|at| app.now.saturating_duration_since(*at) < COPIED);
    let (glyph, tint, tip) = match copied_at {
        Some(_) => (
            Icon::Check,
            SvgClass::Tone(Tone::Success),
            t!("app-copiado-al-portapapeles"),
        ),
        None => (Icon::Copy, SvgClass::Text, t!("host-copiar-codigo")),
    };
    // El pulso: crece un cuarto y vuelve (sin escalas con movimiento reducido).
    let pulse = match copied_at {
        Some(at) if !app.motion().is_reduced() => {
            let t = progress(app, at, Token::Panel, Duration::ZERO);
            0.25 * (t * std::f32::consts::PI).sin()
        }
        _ => 0.0,
    };
    let control = coroom_ui::animate::button(iced_widget::center(icon_with(
        glyph,
        size::ICON * (1.0 + pulse),
        tint,
    )))
    .width(Length::Fixed(COPY_SIZE))
    .height(Length::Fixed(COPY_SIZE))
    .padding(0)
    .class(ButtonClass::Flat)
    .press_scale(true)
    .on_press_maybe(room.code.as_ref().map(|_| on(Msg::CopyCode)));
    ui::with_tooltip(control, tip)
}

/// Cuánto rojo lleva el borde de la transmisión: 1 mientras se comparte; al dejar de compartir se
/// apaga con un fundido de 260 ms (lo que dura [`Token::Screen`]).
fn live_amount(app: &App) -> f32 {
    let motion = &app.host.motion;
    if motion.live_since.is_some() {
        return 1.0;
    }
    let Some(ended) = motion.live_ended else {
        return 0.0;
    };
    let duration = app.motion().duration(Token::Screen);
    if duration.is_zero() {
        return 0.0;
    }
    let t = app.now.saturating_duration_since(ended).as_secs_f32() / duration.as_secs_f32();
    1.0 - EXIT_EASING.value(t.clamp(0.0, 1.0))
}

/// El brillo del arranque de la transmisión (1 al empezar, 0 en reposo): una sola vez, sin nada
/// continuo después (redibujaría la ventana a cada cuadro en el equipo que codifica).
fn live_glow(app: &App) -> f32 {
    app.host.motion.glow_at.map_or(0.0, |at| {
        1.0 - progress(app, at, Token::Moment, Duration::ZERO)
    })
}

/// La etiqueta «EN VIVO» con lo que lleva transmitiendo, en una ranura de ancho fijo.
fn live_tag<'a>(glow: f32, seconds: u64) -> Element<'a, Message> {
    container(
        row![
            ui::badge_text(t!("host-en-vivo").to_owned()).wrapping(Wrapping::None),
            ui::figure(
                ui::badge_text,
                stats::elapsed(seconds),
                &[stats::elapsed_widest(seconds)]
            ),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([3.0, 10.0]))
    .class(ContainerClass::LiveTag { glow })
    .into()
}

/// La vista previa en su sitio de la sala; al abrirla, crece desde el tamaño que tenía en
/// Compartir ([`Token::Moment`]) sin mover lo de al lado (su caja ya tiene el tamaño final).
fn grown_preview<'a>(app: &App, content: Element<'a, Message>) -> Element<'a, Message> {
    let height = (PREVIEW_WIDTH * 9.0 / 16.0).round();
    let grown = app
        .host
        .motion
        .opened_at
        .map_or(1.0, |at| progress(app, at, Token::Moment, Duration::ZERO));
    let from = app.motion().start_scale(THUMB_WIDTH / PREVIEW_WIDTH);
    let scale = from + (1.0 - from) * grown;
    let rise = app.motion().slide(space::M) * (1.0 - grown);
    container(Shift::new(
        thumb(content, (PREVIEW_WIDTH * scale).round()),
        Vector::new(0.0, rise),
    ))
    .center_x(Length::Fixed(PREVIEW_WIDTH))
    .center_y(Length::Fixed(height))
    .into()
}

/// La transmisión, arriba de todo: la vista previa, qué es y con qué se codifica, con el borde
/// rojo y «EN VIVO» mientras sale de este equipo; «Cambiar fuente» (que se abre con su altura) y
/// «Dejar de compartir». Sin nada compartido, «No compartes nada» y el botón para compartir.
fn room_source_card<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let state = &app.host;
    let picker: Element<'a, Message> = Collapse::new(
        container(column![rule::horizontal(1), source_picker(app)].spacing(space::S)).padding(
            Padding {
                top: space::S,
                ..Padding::ZERO
            },
        ),
        state.motion.source.shown(app.now),
    )
    .into();
    let card = |content: Element<'a, Message>| -> Element<'a, Message> {
        container(content)
            .padding(space::M)
            .width(Length::Fill)
            .class(ContainerClass::CardLive {
                live: live_amount(app),
                glow: live_glow(app),
            })
            .into()
    };
    if voice_only(state) {
        let info = column![
            ui::strong(t!("host-no-compartes-nada")),
            ui::secondary(t!("host-no-compartes-nada-detalle")).wrapping(Wrapping::WordOrGlyph),
        ]
        .spacing(2)
        .width(Length::Fill);
        let share = ui::button_with(
            Kind::Primary,
            t!("host-compartir-pantalla-o-ventana"),
            Some(Icon::ShareScreen),
            (!state.changing_source).then(|| on(Msg::StartSharing)),
        );
        let top = row![
            grown_preview(app, placeholder(Icon::Mic, t!("host-solo-voz-y-chat"))),
            info,
        ]
        .spacing(space::M)
        .align_y(Alignment::Center);
        return card(
            column![top, row![ui::fill(), share], picker]
                .spacing(space::S)
                .into(),
        );
    }
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
    let facts = match &room.codec {
        Some(codec) => codec.clone(),
        None if room.guests.is_empty() => t!("host-esperando-al-primer-invitado").to_owned(),
        None => t!("host-eligiendo-el-codec-con-tus-invitados").to_owned(),
    };
    // El bitrate objetivo detrás, en una ranura del ancho del más ancho: cambia con el control de
    // tasa y no debe mover el texto de al lado.
    let rate = room
        .guests
        .iter()
        .filter_map(|g| g.target_kbps)
        .min()
        .map(|kbps| {
            let line = |rate: &str| -> Element<'_, Message> {
                ui::secondary(format!(" · {rate}"))
                    .wrapping(Wrapping::None)
                    .into()
            };
            ui::reserve(
                stats::KBPS_WIDEST.map(line),
                Some(line(&stats::kbps(f64::from(kbps)))),
            )
            .into()
        });
    // «EN VIVO» con su tiempo, en una ranura que está siempre (vacía mientras no sale nada).
    let seconds = state
        .motion
        .live_since
        .map(|since| app.now.saturating_duration_since(since).as_secs());
    let glow = live_glow(app);
    let tag = ui::reserve(
        [live_tag(0.0, seconds.unwrap_or(0))],
        seconds.map(|seconds| live_tag(glow, seconds)),
    );
    let info = column![
        tag,
        ui::one_line(title, ui::strong).width(Length::Fill),
        row![ui::one_line(facts, ui::secondary), ui::slot(rate)].align_y(Alignment::Center),
    ]
    .spacing(space::XS)
    .width(Length::Fill);
    let idle = !state.changing_source && state.switching.is_none();
    let actions = row![
        ui::fill(),
        ui::button_with(
            Kind::Normal,
            t!("host-cambiar-fuente"),
            Some(Icon::ArrowUpDown),
            idle.then(|| on(Msg::ChangeSource(true))),
        ),
        ui::button_with(
            Kind::DestructiveSoft,
            t!("host-dejar-de-compartir"),
            Some(Icon::Pause),
            idle.then(|| on(Msg::StopSharing)),
        ),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    let top = row![grown_preview(app, preview), info]
        .spacing(space::M)
        .align_y(Alignment::Center);
    card(column![top, actions, picker].spacing(space::S).into())
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
            (state.changing_source && state.switching.is_none())
                .then(|| on(Msg::ChangeSource(false))),
        ),
        ui::button_with(
            Kind::Primary,
            t!("host-compartir-esto"),
            None,
            (state.changing_source && !busy && state.source_changed())
                .then(|| on(Msg::ApplySource)),
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

/// Identificador de la cabecera de la fila de `person` en «Personas» (para las pruebas de que
/// nada se mueve).
pub fn person_id(person: Person) -> iced_widget::Id {
    iced_widget::Id::from(match person {
        Person::Me => "person-me".to_owned(),
        Person::Guest(id) => format!("person-{}", id.0),
    })
}

/// Clave de una fila de «Personas»: cada fila conserva su estado (un deslizador que se arrastra,
/// un desplegable abierto) aunque entre o salga otra.
#[derive(Debug, Clone, Copy, PartialEq)]
enum RowKey {
    Me,
    Guest(GuestId),
    Leaving(GuestId),
    Empty,
}

/// Clave de una entrada de la bandeja de peticiones.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TrayKey {
    Request(u64),
    Leaving(u64, Instant),
}

/// «Personas (3)»: el título con la regla de la voz al pasar el ratón, «En pausa» en su ranura y
/// «Pausar teclado y mandos»; las filas; y la bandeja de los que piden entrar.
fn people_section<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let voice_rule = if room.guests_muted {
        t!("host-voz-entran-callados")
    } else {
        t!("host-voz-entran-hablando")
    };
    let narrow = compact(app);
    let pause_button = |paused: bool, enabled: bool| -> Element<'a, Message> {
        if narrow {
            return if paused {
                ui::icon_button(
                    Icon::Play,
                    t!("host-reanudar-teclado-y-mandos"),
                    enabled.then(|| on(Msg::Pause(false))),
                )
            } else {
                ui::icon_button(
                    Icon::Pause,
                    t!("host-pausar-teclado-y-mandos"),
                    enabled.then(|| on(Msg::Pause(true))),
                )
            };
        }
        if paused {
            ui::button_with(
                Kind::Normal,
                t!("host-reanudar-teclado-y-mandos"),
                Some(Icon::Play),
                enabled.then(|| on(Msg::Pause(false))),
            )
            .into()
        } else {
            ui::button_with(
                Kind::Normal,
                t!("host-pausar-teclado-y-mandos"),
                Some(Icon::Pause),
                enabled.then(|| on(Msg::Pause(true))),
            )
            .into()
        }
    };
    let pause = ui::reserve(
        [pause_button(false, true), pause_button(true, true)],
        Some(pause_button(
            room.paused,
            room.paused || !room.guests.is_empty(),
        )),
    )
    .align_x(Horizontal::Right);
    let paused = || {
        ui::with_tooltip(
            ui::status_badge(Tone::Warning, t!("host-en-pausa")),
            t!("host-en-pausa-detalle"),
        )
    };
    let head = row![
        ui::strong(t!("host-personas", valor = room.guests.len() + 1)).size(15.0),
        ui::with_tooltip(
            icon_with(Icon::Info, size::ICON, SvgClass::Secondary),
            voice_rule
        ),
        ui::reserve(
            [ui::status_badge(Tone::Warning, t!("host-en-pausa"))],
            room.paused.then(paused)
        ),
        ui::fill(),
        pause,
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    column![head, people_list(app, room), request_tray(app, room)]
        .spacing(space::XS)
        .into()
}

/// Las filas de «Personas»: primero tú, después los invitados por orden de llegada (quien llega
/// entra deslizándose, quien se va se pliega en su sitio); sin invitados, cómo traerlos.
fn people_list<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let motion = &app.host.motion;
    let fall = app.motion().slide(space::M);
    let mut guests: Vec<(RowKey, Element<'a, Message>, f32)> = room
        .guests
        .iter()
        .map(|status| {
            let entered = motion
                .arrived
                .get(&status.guest)
                .map_or(1.0, |at| progress(app, *at, Token::Panel, Duration::ZERO));
            (
                RowKey::Guest(status.guest),
                guest_row(app, room, status),
                entered,
            )
        })
        .collect();
    let mut leaving: Vec<&LeavingGuest> = motion.leaving.iter().collect();
    leaving.sort_by_key(|leaving| leaving.index);
    for gone in leaving {
        let shown = 1.0 - exit_progress(app, gone.since);
        let at = gone.index.min(guests.len());
        guests.insert(
            at,
            (
                RowKey::Leaving(gone.status.guest),
                guest_row(app, room, &gone.status),
                shown,
            ),
        );
    }
    let mut rows: Vec<(RowKey, Element<'a, Message>, f32)> =
        vec![(RowKey::Me, own_row(app, room), 1.0)];
    rows.extend(guests);
    if room.guests.is_empty() {
        rows.push((
            RowKey::Empty,
            ui::empty_state(
                Icon::Users,
                t!("host-todavia-no-ha-entrado-nadie"),
                t!("host-comparte-el-codigo-de-la-sala"),
                None,
            ),
            1.0,
        ));
    }
    let mut list = iced_widget::keyed::Column::new();
    for (index, (key, element, shown)) in rows.into_iter().enumerate() {
        let element: Element<'a, Message> = if index == 0 {
            element
        } else {
            column![rule::horizontal(1), element].into()
        };
        let element: Element<'a, Message> = if shown >= 1.0 {
            Collapse::new(element, 1.0).into()
        } else {
            Collapse::new(
                Shift::new(element, Vector::new(0.0, -fall * (1.0 - shown))),
                shown,
            )
            .into()
        };
        list = list.push(key, element);
    }
    container(list)
        .width(Length::Fill)
        .class(ContainerClass::Card)
        .into()
}

/// Una fila de «Personas»: la cabecera, de altura fija (nada de lo que llega la cambia), y su
/// desplegable, que crece con su altura solo cuando se abre con el chevrón.
fn person_row<'a>(
    app: &App,
    person: Person,
    header: Element<'a, Message>,
    details: Element<'a, Message>,
) -> Element<'a, Message> {
    let shown = app
        .host
        .motion
        .people
        .get(&person)
        .map_or(0.0, |fold| fold.shown(app.now));
    column![
        container(header)
            .padding(Padding::from([10.0, 16.0]))
            .width(Length::Fill)
            .id(person_id(person)),
        Collapse::new(
            container(details)
                .padding(Padding {
                    top: 0.0,
                    right: 16.0,
                    bottom: 14.0,
                    left: if compact(app) {
                        16.0
                    } else {
                        16.0 + size::AVATAR + 2.0 * coroom_ui::avatar::RING_PADDING + space::S
                    },
                })
                .width(Length::Fill),
            shown,
        ),
    ]
    .into()
}

/// El chevrón que abre el desplegable de `person`: gira con lo abierto.
fn chevron<'a>(app: &App, person: Person, name: &str) -> Element<'a, Message> {
    let shown = app
        .host
        .motion
        .people
        .get(&person)
        .map_or(0.0, |fold| fold.shown(app.now));
    let glyph = iced_widget::svg::Svg::<'a, coroom_ui::Theme>::new(Icon::ChevronDown.handle())
        .width(Length::Fixed(size::ICON))
        .height(Length::Fixed(size::ICON))
        .rotation(Rotation::Floating(Radians(shown * std::f32::consts::PI)))
        .class(SvgClass::Secondary);
    let control = coroom_ui::animate::button(iced_widget::center(glyph))
        .width(Length::Fixed(size::CONTROL))
        .height(Length::Fixed(size::CONTROL))
        .padding(0)
        .class(ButtonClass::Flat)
        .press_scale(true)
        .on_press(on(Msg::Expand(person)));
    ui::with_tooltip(control, t!("host-opciones-de", name = name))
}

/// «Hablando», en una ranura que está siempre (vacía si no habla): la fila no se mueve.
fn speaking_slot<'a>(talking: bool, compact: bool) -> Element<'a, Message> {
    if compact {
        // En la sala estrecha, un punto (el anillo del avatar ya lo dice) con su nombre encima.
        let speaking = || ui::with_tooltip(ui::dot(Tone::Success), t!("host-hablando"));
        return ui::reserve([speaking()], talking.then(speaking)).into();
    }
    let speaking = || ui::status_badge(Tone::Success, t!("host-hablando"));
    ui::reserve([speaking()], talking.then(speaking)).into()
}

/// La latencia de un invitado, siempre visible, con la medida y la escala de toda la aplicación
/// ([`stats::latency`]) y su nombre al pasar el ratón; «— ms» hasta la primera medida.
fn latency_badge<'a>(rtt_ms: Option<f64>, with_tip: bool) -> Element<'a, Message> {
    let (tone, value, tip) = match rtt_ms {
        Some(rtt) => {
            let (value, level) = stats::latency(rtt);
            (
                quality_tone(level),
                value,
                t!("host-latencia-detalle", level = quality_label(level)),
            )
        }
        None => (
            Tone::Neutral,
            "— ms".to_owned(),
            t!("host-latencia-midiendo").to_owned(),
        ),
    };
    let badge = ui::status_badge_with(
        tone,
        ui::figure(ui::badge_text, value, &[stats::LATENCY_WIDEST]),
    );
    if with_tip {
        ui::with_tooltip(badge, tip)
    } else {
        badge
    }
}

/// El botón de silenciarse de tu fila (`None` en `voice`: sin micrófono en la sala).
fn mute_button<'a>(voice: Option<bool>, compact: bool) -> Element<'a, Message> {
    if compact {
        // En la sala estrecha, solo el icono (con lo que hace al pasar el ratón).
        return match voice {
            None => ui::icon_button(Icon::MicOff, t!("app-sin-microfono"), None),
            Some(true) => ui::icon_button(
                Icon::MicOff,
                t!("host-activar-tu-microfono"),
                Some(on(Msg::SelfMute(false))),
            ),
            Some(false) => ui::icon_button(
                Icon::Mic,
                t!("host-silenciarte"),
                Some(on(Msg::SelfMute(true))),
            ),
        };
    }
    match voice {
        None => ui::button_with(
            Kind::Normal,
            t!("app-sin-microfono"),
            Some(Icon::MicOff),
            None,
        )
        .into(),
        Some(true) => ui::with_tooltip(
            ui::button_with(
                Kind::Normal,
                t!("host-activar"),
                Some(Icon::MicOff),
                Some(on(Msg::SelfMute(false))),
            ),
            t!("host-activar-tu-microfono"),
        ),
        Some(false) => ui::with_tooltip(
            ui::button_with(
                Kind::Normal,
                t!("host-silenciar"),
                Some(Icon::Mic),
                Some(on(Msg::SelfMute(true))),
            ),
            t!("host-silenciarte"),
        ),
    }
}

/// La columna del final de cada fila (tu botón de silenciarte o la latencia del invitado), del
/// ancho de la más ancha: «Hablando» y el chevrón quedan alineados en todas las filas.
fn trailing<'a>(content: Element<'a, Message>, compact: bool) -> Element<'a, Message> {
    ui::reserve(
        [
            latency_badge(None, false),
            mute_button(None, compact),
            mute_button(Some(true), compact),
            mute_button(Some(false), compact),
        ],
        Some(content),
    )
    .align_x(Horizontal::Right)
    .into()
}

/// Una fila de detalle del desplegable: el título y su ayuda a la izquierda, el control a la
/// derecha.
fn detail_row<'a>(
    title: impl iced_core::text::IntoFragment<'a>,
    help: impl iced_core::text::IntoFragment<'a>,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![
        column![
            ui::body(title),
            ui::caption(help).wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(2)
        .width(Length::Fill),
        control.into()
    ]
    .spacing(space::M)
    .align_y(Alignment::Center)
    .into()
}

/// Deslizador de volumen con el altavoz delante: tachado y con la barra apagada si está
/// silenciado (el valor se conserva).
fn volume_slider<'a>(
    width: f32,
    percent: u8,
    muted: bool,
    (icon, muted_icon): (Icon, Icon),
    (speaker_tip, on_speaker): (String, Message),
    on_change: impl Fn(u8) -> Message + 'a,
    on_release: Message,
) -> Element<'a, Message> {
    let speaker = ui::icon_button(
        if muted { muted_icon } else { icon },
        speaker_tip,
        Some(on_speaker),
    );
    container(ui::slider_with(
        0..=MAX_VOICE_VOLUME,
        percent,
        5,
        DEFAULT_VOICE_VOLUME,
        Some(speaker),
        t!("host-volumen-porcentaje", percent = percent),
        SliderClass::Accent.muted(muted),
        on_change,
        Some(on_release),
    ))
    .width(Length::Fixed(width))
    .into()
}

/// Ancho del deslizador de volumen del desplegable (en la sala estrecha, [`VOLUME_WIDTH_COMPACT`]).
const VOLUME_WIDTH: f32 = 260.0;

/// Ancho del deslizador de volumen en la sala estrecha.
const VOLUME_WIDTH_COMPACT: f32 = 170.0;

fn volume_width(app: &App) -> f32 {
    if compact(app) {
        VOLUME_WIDTH_COMPACT
    } else {
        VOLUME_WIDTH
    }
}

/// El ancho que le queda a la página de la sala, sin el riel ni el chat.
fn room_width(app: &App) -> f32 {
    let rail = if ui::rail_is_wide(app.window.width) {
        ui::RAIL_WIDE
    } else {
        ui::RAIL_NARROW
    };
    app.window.width - rail - (CHAT_WIDTH + space::M + space::S)
}

/// Por debajo de este ancho de página, la sala va en su forma estrecha.
const COMPACT_BELOW: f32 = 640.0;

/// La sala estrecha (la ventana estrecha con el chat al lado): la cabecera en dos pisos, los
/// botones de las filas solo con su icono y «Hablando» como un punto, para que los nombres
/// quepan. Depende solo del ancho de la ventana: nada que llegue cambia la forma.
fn compact(app: &App) -> bool {
    room_width(app) < COMPACT_BELOW
}

/// Tu fila: «Ana (tú)» con «Anfitrión», cómo está tu micrófono, «Hablando» y el botón para
/// silenciarte; en el desplegable, lo alto que te oyen.
fn own_row<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let name = app
        .account
        .as_ref()
        .map_or_else(|| t!("host-tu").to_owned(), |a| a.display_name.clone());
    let voice = &room.voice;
    let talking = voice.talkers.contains(&0);
    let status = if !voice.enabled {
        t!("app-sin-microfono-detalle")
    } else if voice.mic_muted {
        t!("host-te-has-silenciado")
    } else {
        t!("app-microfono-abierto")
    };
    let header = row![
        avatars::ringed_view(
            app,
            &name,
            app.account.as_ref().and_then(|account| account.avatar),
            size::AVATAR,
            talking,
        ),
        column![
            row![
                ui::one_line(t!("host-tu-nombre", name = name.as_str()), ui::strong),
                coroom_ui::tag::live(t!("host-anfitrion")),
            ]
            .spacing(space::XS)
            .align_y(Alignment::Center),
            ui::one_line(status, ui::secondary).width(Length::Fill)
        ]
        .spacing(2)
        .width(Length::Fill),
        speaking_slot(talking, compact(app)),
        trailing(
            mute_button(voice.enabled.then_some(voice.mic_muted), compact(app)),
            compact(app)
        ),
        chevron(app, Person::Me, &name),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    let details: Element<'a, Message> = if voice.enabled {
        let muted = voice.mic_muted;
        detail_row(
            t!("host-tu-voz"),
            t!("host-tu-voz-ayuda"),
            volume_slider(
                volume_width(app),
                app.settings.audio.mic_volume,
                muted,
                (Icon::Mic, Icon::MicOff),
                (
                    if muted {
                        t!("host-activar-tu-microfono").to_owned()
                    } else {
                        t!("host-silenciarte").to_owned()
                    },
                    on(Msg::SelfMute(!muted)),
                ),
                |value| on(Msg::MicVolume(value)),
                on(Msg::MicVolumeDone),
            ),
        )
    } else {
        ui::secondary(t!("host-sin-microfono-en-la-sala"))
            .wrapping(Wrapping::WordOrGlyph)
            .into()
    };
    person_row(app, Person::Me, header.into(), details)
}

/// La fila de un invitado: quién, cómo llega y qué puede hacer en una línea («…» si no cabe),
/// «Hablando» y su latencia; en el desplegable, el volumen con que se le oye aquí, sus permisos,
/// su mando, el número de seguridad, pasarle la sala y expulsarlo.
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
    facts.push(permissions_label(&permissions));
    if !status.input_key && (permissions.keyboard_mouse || permissions.gamepad_slot.is_some()) {
        facts.push(t!("host-esperando-la-clave").to_owned());
    }
    let talking = room.voice.talkers.contains(&id.0);
    let header = row![
        avatars::ringed_view(app, &status.name, status.avatar, size::AVATAR, talking),
        column![
            ui::one_line(status.name.clone(), ui::strong),
            ui::one_line(facts.join(" · "), ui::secondary).width(Length::Fill)
        ]
        .spacing(2)
        .width(Length::Fill),
        speaking_slot(talking, compact(app)),
        trailing(latency_badge(status.rtt_ms, true), compact(app)),
        chevron(app, Person::Guest(id), &status.name),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    person_row(
        app,
        Person::Guest(id),
        header.into(),
        guest_details(app, room, status),
    )
}

/// El desplegable de un invitado. Tiene siempre las mismas filas (el mando, su VID:PID y sus
/// notas en ranuras): lo que llega no le cambia la altura.
fn guest_details<'a>(
    app: &'a App,
    room: &'a Room,
    status: &'a GuestStatus,
) -> Element<'a, Message> {
    let id = status.guest;
    let permissions = status.permissions;
    let muted = hears_muted(room, id);
    let volume = detail_row(
        t!("host-volumen"),
        t!("host-volumen-ayuda"),
        volume_slider(
            volume_width(app),
            room.volume(id),
            muted,
            (Icon::Volume, Icon::VolumeOff),
            (
                if muted {
                    t!("host-volver-a-oir", name = status.name.as_str())
                } else {
                    t!("host-dejar-de-oir", name = status.name.as_str())
                },
                on(Msg::Hear(id, muted)),
            ),
            move |value| on(Msg::Volume(id, value)),
            on(Msg::VolumeDone(id)),
        ),
    );
    let keyboard = detail_row(
        t!("host-teclado-y-raton"),
        t!("host-teclado-y-raton-ayuda"),
        ui::switch(
            permissions.keyboard_mouse,
            Some(move |on_off| on(Msg::Keyboard(id, on_off))),
        ),
    );
    let voice = detail_row(
        t!("host-voz"),
        t!("host-voz-permiso-ayuda"),
        ui::switch(
            permissions.voice,
            Some(move |on_off| on(Msg::GuestVoice(id, on_off))),
        ),
    );
    let handoff = app.host.handoff.is_none();
    let kick = ui::button_with(
        Kind::DestructiveSoft,
        t!("host-expulsar"),
        Some(Icon::UserX),
        Some(on(Msg::Kick(id))),
    );
    // «Expulsar», en rojo y separado del resto por una línea (en la sala estrecha baja a su
    // línea y la línea sobra).
    let kick: Element<'a, Message> = if compact(app) {
        kick.into()
    } else {
        row![
            container(rule::vertical(1)).height(Length::Fixed(size::CONTROL - 12.0)),
            kick,
        ]
        .spacing(space::S)
        .align_y(Alignment::Center)
        .into()
    };
    let actions = row![
        ui::button_with(
            Kind::Normal,
            t!("host-numero-de-seguridad"),
            Some(Icon::Fingerprint),
            Some(on(Msg::Safety(id))),
        ),
        ui::button_with(
            Kind::Normal,
            t!("host-pasarle-la-sala"),
            Some(Icon::ArrowRight),
            handoff.then(|| on(Msg::Handoff(id))),
        ),
        kick,
    ]
    .spacing(space::S)
    .align_y(Alignment::Center)
    .wrap()
    .vertical_spacing(space::XS);
    column![volume, keyboard, voice, pad_line(app, status), actions]
        .spacing(space::S)
        .into()
}

/// El mando de un invitado: la ranura («Sin mando» se lo quita), si va tal cual o convertido, y
/// el mando que declara (nombre, VID:PID y cómo llega) con, si no va tal cual pudiendo, por qué.
/// El VID:PID y las notas van en ranuras: aparecen sin cambiar la altura.
fn pad_line<'a>(app: &'a App, status: &'a GuestStatus) -> Element<'a, Message> {
    let id = status.guest;
    let max = coroom_session::input::max_gamepads();
    let slots = pad_choices(app.settings.input.preferred_slot, max);
    let current = PadSlot(status.permissions.gamepad_slot);
    let (name, note, badge) = match &status.gamepad {
        None => (
            t!("host-todavia-no-ha-conectado-ningun-mando").to_owned(),
            None,
            None,
        ),
        Some(pad) => {
            let transport = match pad.transport {
                PadTransport::Hid => "HID",
                PadTransport::XInput => "XInput",
                PadTransport::Converted => t!("host-solo-convertido"),
            };
            (
                t!("host-su-mando-name", name = pad.name.as_str()),
                pad.note
                    .as_ref()
                    .map(|note| t!("host-va-convertido-note", note = note.as_str())),
                Some(format!(
                    "{:04x}:{:04x} · {transport}",
                    pad.vendor, pad.product
                )),
            )
        }
    };
    let pad_badge = |label: String| ui::status_badge(Tone::Neutral, label);
    let texts = column![
        row![
            ui::body(t!("host-mando")),
            ui::reserve(
                [pad_badge("8888:8888 · XInput".to_owned())],
                badge.map(pad_badge)
            ),
        ]
        .spacing(space::XS)
        .align_y(Alignment::Center),
        ui::one_line(name, ui::caption).width(Length::Fill),
        ui::reserve(
            [ui::caption(" ").into()],
            note.map(|note| ui::one_line(note, ui::caption).width(Length::Fill).into()),
        ),
    ]
    .spacing(2)
    .width(Length::Fill);
    let selects = row![
        ui::select(slots, Some(current), move |slot| on(Msg::Pad(id, slot))),
        ui::select(
            vec![PadMode(true), PadMode(false)],
            Some(PadMode(status.permissions.gamepad_raw)),
            move |mode| on(Msg::PadMode(id, mode)),
        ),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    if compact(app) {
        return column![texts, row![ui::fill(), selects]]
            .spacing(space::XS)
            .into();
    }
    row![texts, selects]
        .spacing(space::S)
        .align_y(Alignment::Center)
        .into()
}

/// La bandeja de los que piden entrar, bajo «Personas»: cada petición cae al llegar y se pliega
/// al contestarla o al vencer; no empuja nada de arriba.
fn request_tray<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let mut list = iced_widget::keyed::Column::new();
    for request in &app.host.join_requests {
        list = list.push(
            TrayKey::Request(request.user),
            tray_entry(
                app,
                join_request_card(app, room, request, false),
                app.host.motion.request_entered(app, request.at),
                0.0,
            ),
        );
    }
    for leaving in &app.host.motion.requests_leaving {
        list = list.push(
            TrayKey::Leaving(leaving.request.user, leaving.since),
            tray_entry(
                app,
                join_request_card(app, room, &leaving.request, true),
                1.0,
                exit_progress(app, leaving.since),
            ),
        );
    }
    list.into()
}

/// Una entrada de la bandeja con su movimiento: al llegar (`entered` de 0 a 1) crece y cae desde
/// arriba; al irse (`left` de 0 a 1) se pliega. El espacio de encima va dentro: plegada no ocupa
/// nada.
fn tray_entry<'a>(
    app: &App,
    content: Element<'a, Message>,
    entered: f32,
    left: f32,
) -> Element<'a, Message> {
    let fall = app.motion().slide(space::M) * (1.0 - entered);
    Collapse::new(
        Shift::new(
            container(content).padding(Padding {
                top: space::XS,
                ..Padding::ZERO
            }),
            Vector::new(0.0, -fall),
        ),
        entered * (1.0 - left),
    )
    .into()
}

/// Tarjeta de un amigo que pide entrar, con el borde del acento, su cuenta atrás y una barra fina
/// del tiempo que queda: «Dejar entrar» lo invita (en cuanto la sala tiene código) y «Ahora no»
/// se lo dice. `leaving`: se va (sin botones que hagan nada).
fn join_request_card<'a>(
    app: &'a App,
    room: &'a Room,
    request: &'a JoinRequest,
    leaving: bool,
) -> Element<'a, Message> {
    let ago = app
        .now
        .saturating_duration_since(request.at)
        .min(JOIN_REQUEST_TTL);
    let remaining = JOIN_REQUEST_TTL.saturating_sub(ago);
    let left = remaining.as_secs().max(u64::from(!leaving));
    let fraction = remaining.as_secs_f32() / JOIN_REQUEST_TTL.as_secs_f32();
    let decline = ui::flat(
        t!("host-ahora-no"),
        (!leaving).then(|| on(Msg::DeclineJoin(request.user))),
    );
    let allow = ui::button_with(
        Kind::Primary,
        t!("host-dejar-entrar"),
        Some(Icon::LogIn),
        room.code
            .as_ref()
            .filter(|_| !leaving)
            .map(|_| on(Msg::AllowJoin(request.user))),
    );
    // En la sala estrecha, los botones en su línea: el nombre no cabía a su lado.
    let (inline, below): (Vec<Element<'a, Message>>, Option<Element<'a, Message>>) = if compact(app)
    {
        (
            Vec::new(),
            Some(
                row![ui::fill(), decline, allow]
                    .spacing(space::S)
                    .align_y(Alignment::Center)
                    .into(),
            ),
        )
    } else {
        (vec![decline.into(), allow.into()], None)
    };
    container(
        column![
            row![
                avatars::view(
                    app,
                    &request.name,
                    avatars::of_user(app, request.user),
                    size::AVATAR,
                ),
                column![
                    ui::one_line(t!("host-name-pide-entrar", name = request.name), ui::strong)
                        .width(Length::Fill),
                    // La cuenta atrás en m:ss en una ranura de ancho fijo: cambia cada segundo
                    // sin mover el texto ni cortar la línea.
                    row![
                        ui::secondary(t!("host-caduca-en")).wrapping(Wrapping::None),
                        ui::figure(ui::secondary, stats::clock(left), &[stats::CLOCK_WIDEST]),
                    ]
                    .spacing(space::XXS)
                ]
                .spacing(2)
                .width(Length::Fill),
            ]
            .extend(inline)
            .spacing(space::S)
            .align_y(Alignment::Center),
            ui::slot(below),
            progress_bar(0.0..=1.0, fraction)
                .girth(Length::Fixed(TIME_BAR))
                .class(ProgressClass::Accent),
        ]
        .spacing(space::S),
    )
    .padding(Padding::from([12.0, 16.0]))
    .width(Length::Fill)
    .class(ContainerClass::CardAccent)
    .into()
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
                Some(avatars::view(
                    app,
                    &friend.display_name,
                    friend.avatar,
                    28.0,
                )),
                friend.display_name.as_str(),
                None,
                vec![
                    super::home::activity_badge(friend),
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
        .width(Length::Fixed(CHAT_WIDTH + space::M + space::S))
        .height(Length::Fill)
        .padding(Padding {
            top: space::M,
            right: space::M,
            bottom: space::M,
            // Aire entre la barra de desplazamiento de la página y el chat.
            left: space::S,
        })
        .into()
}

/// Con la sala abierta: la cabecera, la transmisión, las personas (con las solicitudes debajo),
/// el audio, los dispositivos, la calidad y a quién más invitar. Todos los bloques están siempre,
/// en el mismo orden: lo que aparece dentro de uno no cambia de posición entre los hijos a los
/// de después, y sus desplegables y deslizadores no pierden el estado.
fn room_view<'a>(app: &'a App, room: &'a Room) -> Element<'a, Message> {
    let mut notices = Vec::new();
    if let Some(relay) = &room.relay {
        notices.push(ui::banner(
            Tone::Warning,
            t!("host-sin-relay-relay-solo-podran-entrar", relay = relay),
            None,
        ));
    }
    if let Some((_, name)) = &app.host.handoff {
        // Pasar la sala, a la vista mientras dura: retirable hasta que conteste.
        notices.push(if app.host.handing_over {
            ui::banner(
                Tone::Neutral,
                t!("host-pasando-la-sala-a", name = name.as_str()),
                None,
            )
        } else {
            ui::banner(
                Tone::Neutral,
                t!("host-esperando-a-que-acepte", name = name.as_str()),
                Some((t!("host-retirar-la-oferta"), on(Msg::CancelHandoff))),
            )
        });
    }
    // Los avisos van en el bloque de la cabecera, en una ranura que está siempre (con el espacio
    // de encima dentro).
    let notices = (!notices.is_empty()).then(|| {
        container(Column::with_children(notices).spacing(space::M))
            .padding(Padding {
                top: space::M,
                ..Padding::ZERO
            })
            .into()
    });
    let page = Column::new()
        .spacing(space::M)
        .push(column![room_header(app, room), ui::slot(notices)])
        .push(room_source_card(app, room))
        .push(people_section(app, room))
        .push(audio_box(app, false))
        .push(devices_box(app))
        .push(room_quality(app))
        .push(ui::slot(invite_more(app, room)));
    Row::new()
        .push(container(ui::page("room", page)).width(Length::Fill))
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

    use std::time::Duration;

    use super::{Msg, Person, ShareMode, ShareOptions, SourceChoice, TargetOption};
    use crate::ui::app::{App, Dialog, Event, HostVoice, Message, Page};
    use crate::ui::settings::QualityPreset;
    use crate::ui::shots::{Builder, NARROW, WIDE, game_frame, playing, safety, signed_in};
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

    /// Compartir la pantalla, con Bea marcada para invitarla y Carla jugando en otra sala.
    pub(crate) fn host_setup(app: &mut App) {
        signed_in(app, Page::Host);
        app.update(Message::Event(Event::Activity {
            user: 3,
            activity: coroom_transport::proto::signal::Activity::Playing,
        }));
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

    /// Una ventana, con «Avanzado» desplegado y el perfil personalizado.
    pub(crate) fn host_setup_advanced(app: &mut App) {
        signed_in(app, Page::Host);
        app.update(Message::Host(Msg::Mode(ShareMode::Window)));
        windows(app);
        app.update(Message::Host(Msg::Preset(QualityPreset::MinLatency)));
        app.update(Message::Host(Msg::Fps(120)));
        app.update(Message::Host(Msg::Advanced(true)));
        super::settle(app);
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
            avatar: crate::ui::shots::sample_avatar(name),
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
        app.update(Message::Event(Event::HostGuests(
            vec![
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
                        // Un mando de ejemplo (ni nombre ni VID:PID de un modelo de verdad).
                        name: "Mando inalámbrico de ejemplo".to_owned(),
                        vendor: 0x1234,
                        product: 0x5678,
                        transport: PadTransport::Hid,
                        raw: true,
                        note: None,
                    },
                ),
            ],
            HostVoice {
                enabled: true,
                mic_muted: false,
                talkers: vec![1],
            },
        )));
        app.update(Message::Host(Msg::Volume(GuestId(2), 60)));
        app.update(Message::Host(Msg::Hear(GuestId(2), false)));
        // Transmitiendo desde hace 12:04, con la apertura ya terminada.
        app.host.motion.live_since = app.now.checked_sub(Duration::from_secs(12 * 60 + 4));
        super::settle(app);
        app.toasts.clear();
    }

    /// La sala con el desplegable de `person` abierto (por el usuario, como siempre).
    fn expanded(app: &mut App, person: Person) {
        app.update(Message::Host(Msg::Expand(person)));
        super::settle(app);
    }

    fn window(app: &mut App, (width, height): (u32, u32)) {
        app.window = iced_core::Size::new(width as f32, height as f32);
    }

    /// Con la sala abierta, eligiendo la ventana de un juego en vez de la pantalla.
    pub(crate) fn host_room_switch(app: &mut App) {
        host_room(app);
        app.update(Message::Host(Msg::ChangeSource(true)));
        app.update(Message::Host(Msg::Mode(ShareMode::Window)));
        // Un título largo, como los de Windows: no debe aplastar el selector.
        let game = coroom_session::capture::CaptureTarget::Window {
            id: 7,
            title: "Juego de lucha de ejemplo — Edición especial definitiva (DirectX 12)"
                .to_owned(),
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
        super::settle(app);
        app.toasts.clear();
    }

    pub(crate) fn host_stop_dialog(app: &mut App) {
        host_room(app);
        app.dialog = Some(Dialog::StopSharing);
    }

    /// La sala abierta con Dani (conectado, sin sala) pidiendo entrar desde hace 6 s.
    pub(crate) fn host_room_join_request(app: &mut App) {
        host_room(app);
        app.update(Message::Event(Event::Presence {
            user: 4,
            online: true,
        }));
        app.update(Message::Event(Event::JoinRequested {
            from_user: 4,
            from_name: "Dani".to_owned(),
        }));
        if let Some(request) = app.host.join_requests.first_mut() {
            request.at = app
                .now
                .checked_sub(std::time::Duration::from_secs(6))
                .unwrap_or(app.now);
        }
        app.toasts.clear();
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[
        ("08-compartir", host_setup),
        ("08-compartir-ventana", host_setup_window),
        ("08-compartir-avanzado", host_setup_advanced),
        ("08-compartir-wayland", host_setup_wayland),
        ("08-compartir-solo-voz", |app| {
            host_setup(app);
            app.update(Message::Host(Msg::Mode(super::ShareMode::Nothing)));
        }),
        ("09-sala-abierta", host_room),
        ("09-sala-sin-compartir", |app| {
            host_room(app);
            app.update(Message::Host(Msg::StopSharing));
            app.update(Message::Event(Event::SourceSwitched(Ok(()))));
            super::settle(app);
            app.toasts.clear();
        }),
        ("09-sala-persona-abierta", |app| {
            host_room(app);
            expanded(app, Person::Guest(GuestId(2)));
        }),
        ("09-sala-tu-fila-abierta", |app| {
            host_room(app);
            expanded(app, Person::Me);
        }),
        ("09-sala-ancha", |app| {
            host_room_join_request(app);
            window(app, WIDE);
            expanded(app, Person::Guest(GuestId(1)));
        }),
        ("09-sala-estrecha", |app| {
            host_room_join_request(app);
            window(app, NARROW);
            expanded(app, Person::Guest(GuestId(2)));
        }),
        ("09-sala-cambiar-fuente", host_room_switch),
        ("09-sala-peticion-de-entrada", host_room_join_request),
        ("09-sala-pasando", |app| {
            host_room(app);
            let first = app.host.room.as_ref().and_then(|room| room.guests.first());
            if let Some((guest, name)) = first.map(|status| (status.guest, status.name.clone())) {
                super::offer_handoff(app, guest, name);
            }
            super::settle(app);
        }),
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
            avatar: None,
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
        app.update(Message::Event(Event::HostGuests(
            vec![status(1, "Bea"), status(2, "Carla")],
            HostVoice::default(),
        )));
    }

    /// Compartiendo, el escritorio del anfitrión no se bloquea (quien juega desde lejos con un mando
    /// no cuenta como actividad); en una sala solo de voz, sí.
    #[test]
    fn the_screen_does_not_lock_while_sharing() {
        let mut app = app();
        assert!(!app.wants_idle_inhibit());
        open_room(&mut app);
        assert!(app.wants_idle_inhibit());
        app.update(Message::Host(Msg::StopSharing));
        assert!(!app.wants_idle_inhibit(), "solo voz: nada que capturar");
    }

    /// Con la sala abierta, cambiar de servidor pregunta antes (la cierra para todos) y no se
    /// puede entrar en otra sala con un código ni con una invitación.
    #[test]
    fn an_open_room_is_not_left_by_accident() {
        let mut app = app();
        app.account = Some(crate::ui::app::tests::account());
        open_room(&mut app);
        let effects = app.update(Message::SwitchServer);
        assert!(effects.is_empty(), "{effects:?}");
        assert!(matches!(
            app.dialog,
            Some(crate::ui::app::Dialog::SwitchServerWhileHosting)
        ));
        app.update(Message::CloseDialog);
        assert!(app.host.is_open(), "cancelar deja la sala abierta");

        use crate::ui::screens::home::Msg as HomeMsg;
        app.update(Message::Home(HomeMsg::Code("K7Q2MZ".into())));
        assert!(app.update(Message::Home(HomeMsg::Join)).is_empty());
        assert!(
            app.update(Message::Home(HomeMsg::Accept("K7Q2MZ".into())))
                .is_empty()
        );
        assert!(app.guest.is_none());

        let effects = app.update(Message::SwitchServer);
        assert!(effects.is_empty());
        let effects = app.update(Message::ConfirmDialog);
        assert!(
            effects.iter().any(|e| matches!(e, Effect::StopHost)),
            "{effects:?}"
        );
    }

    /// Pasar la sala: se confirma, se ofrece, Bea contesta con su código, se manda a los demás y,
    /// al cerrar esta, se entra en la suya.
    #[test]
    fn the_host_hands_the_room_over_and_joins_the_new_one() {
        let mut app = app();
        app.account = Some(crate::ui::app::tests::account());
        open_room(&mut app);
        app.update(Message::Host(Msg::GuestMenu(
            GuestId(1),
            GuestAction::Handoff,
        )));
        assert!(matches!(&app.dialog, Some(Dialog::OfferHandoff { name, .. }) if name == "Bea"));
        assert_eq!(
            app.update(Message::ConfirmDialog),
            [Effect::OfferHandoff(GuestId(1))]
        );
        // Otro invitado no puede contestar por Bea: si abrió una sala, se le dice que la cierre.
        assert_eq!(
            app.update(Message::Event(Event::Host(SessionEvent::HandoffReply {
                guest: GuestId(2),
                code: Some("ABCDEF".into()),
            }))),
            [Effect::CancelHandoff(GuestId(2))]
        );
        assert_eq!(
            app.update(Message::Event(Event::Host(SessionEvent::HandoffReply {
                guest: GuestId(1),
                code: Some("ABCDEF".into()),
            }))),
            [Effect::HandOver {
                code: "ABCDEF".into(),
                host_name: "Bea".into(),
            }]
        );
        // Pasándola ya no se puede retirar, y que Bea salga (va a la suya) no la deshace.
        assert!(app.update(Message::Host(Msg::CancelHandoff)).is_empty());
        app.update(Message::Event(Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(1),
            kicked: false,
        })));
        assert!(app.host.handoff.is_some() && app.host.handing_over);
        assert_eq!(
            app.update(Message::Event(Event::HandedOver(Ok("ABCDEF".into())))),
            [Effect::StopHost]
        );
        let effects = app.update(Message::Event(Event::HostEnded(None)));
        assert_eq!(effects, [Effect::StopLobby, Effect::Join("ABCDEF".into())]);
        assert_eq!(app.screen, Screen::Guest);
        assert!(app.host.room.is_none());
        assert!(app.host.handoff.is_none() && !app.host.handing_over);
    }

    /// La oferta pendiente se ve en la sala y se puede retirar; una respuesta que llega después
    /// recibe la retirada (y no pasa la sala). Si quien la tenía sale, se acaba sola.
    #[test]
    fn a_pending_handoff_can_be_withdrawn_or_ends_when_the_guest_leaves() {
        let mut app = app();
        open_room(&mut app);
        app.update(Message::Host(Msg::Handoff(GuestId(1))));
        app.update(Message::ConfirmDialog);
        assert_eq!(app.host.handoff, Some((GuestId(1), "Bea".to_owned())));
        assert_eq!(
            app.update(Message::Host(Msg::CancelHandoff)),
            [Effect::CancelHandoff(GuestId(1))]
        );
        assert!(app.host.handoff.is_none());
        assert_eq!(
            app.update(Message::Event(Event::Host(SessionEvent::HandoffReply {
                guest: GuestId(1),
                code: Some("ABCDEF".into()),
            }))),
            [Effect::CancelHandoff(GuestId(1))]
        );
        // Un «no» tardío no necesita respuesta.
        assert!(
            app.update(Message::Event(Event::Host(SessionEvent::HandoffReply {
                guest: GuestId(1),
                code: None,
            })))
            .is_empty()
        );
        // Otra oferta: quien la tenía se va sin contestar y se puede ofrecer a otro.
        app.update(Message::Host(Msg::Handoff(GuestId(2))));
        app.update(Message::ConfirmDialog);
        app.update(Message::Event(Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(2),
            kicked: false,
        })));
        assert!(app.host.handoff.is_none());
        app.update(Message::Host(Msg::Handoff(GuestId(1))));
        assert!(matches!(&app.dialog, Some(Dialog::OfferHandoff { name, .. }) if name == "Bea"));
    }

    /// El silencio propio del anfitrión pasa a la sala nueva al entrar en ella como invitado.
    #[test]
    fn the_hosts_own_mute_follows_it_to_the_new_room() {
        let mut app = app();
        app.account = Some(crate::ui::app::tests::account());
        open_room(&mut app);
        app.update(Message::Host(Msg::SelfMute(true)));
        app.update(Message::Host(Msg::Handoff(GuestId(1))));
        app.update(Message::ConfirmDialog);
        app.update(Message::Event(Event::Host(SessionEvent::HandoffReply {
            guest: GuestId(1),
            code: Some("ABCDEF".into()),
        })));
        app.update(Message::Event(Event::HandedOver(Ok("ABCDEF".into()))));
        app.update(Message::Event(Event::HostEnded(None)));
        let effects = app.update(Message::Event(Event::GuestStarted(Ok(
            VideoFeed::detached(),
        ))));
        assert!(
            effects.contains(&Effect::SetMicMuted {
                host: false,
                muted: true
            }),
            "{effects:?}"
        );
    }

    /// Callarse se ve al momento y no parpadea con una lista tomada antes del clic; hasta la
    /// primera lista, la voz propia sale como se pidió al abrir (no «sin micrófono»).
    #[test]
    fn the_own_mute_does_not_flicker_and_the_voice_shows_from_the_start() {
        let mut app = app();
        app.settings.audio.host_voice = true;
        app.update(Message::Host(Msg::Start));
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        let voice = |app: &App| app.host.room.as_ref().expect("sala").voice.clone();
        assert!(voice(&app).enabled, "con la voz pedida, desde el principio");
        assert!(!voice(&app).mic_muted);
        app.update(Message::Host(Msg::SelfMute(true)));
        assert!(voice(&app).mic_muted);
        let listed = |muted: bool| HostVoice {
            enabled: true,
            mic_muted: muted,
            talkers: Vec::new(),
        };
        // Una lista de antes del clic no lo deshace.
        app.update(Message::Event(Event::HostGuests(Vec::new(), listed(false))));
        assert!(voice(&app).mic_muted);
        // La que ya lo trae lo confirma; desde ahí, mandan las listas.
        app.update(Message::Event(Event::HostGuests(Vec::new(), listed(true))));
        assert!(voice(&app).mic_muted);
        app.update(Message::Event(Event::HostGuests(Vec::new(), listed(false))));
        assert!(!voice(&app).mic_muted);
    }

    /// Si el invitado no acepta, se queda todo como estaba.
    #[test]
    fn a_declined_handoff_changes_nothing() {
        let mut app = app();
        open_room(&mut app);
        app.update(Message::Host(Msg::Handoff(GuestId(2))));
        app.update(Message::ConfirmDialog);
        assert!(
            app.update(Message::Event(Event::Host(SessionEvent::HandoffReply {
                guest: GuestId(2),
                code: None,
            })))
            .is_empty()
        );
        assert!(app.host.handoff.is_none());
        assert!(app.host.room.is_some());
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text == "Carla no aceptó la sala.")
        );
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
            activity: Default::default(),
            avatar: None,
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

    /// Una ventana manda el audio de todas las aplicaciones salvo las quitadas, y quitar una se
    /// recuerda (también sin que la ventana diga de qué aplicación es).
    #[test]
    fn sharing_a_window_sends_every_app_but_the_removed_ones() {
        let mut app = app();
        app.update(Message::Navigate(Page::Host));
        assert_eq!(
            app.update(Message::Host(Msg::Mode(ShareMode::Window))),
            [Effect::LoadShareOptions(SourceChoice::App)]
        );
        let game = window(7, "Juego", Some("juego.exe"));
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: vec![game.clone(), window(8, "Navegador", Some("firefox.exe"))],
            targets_error: None,
            apps: Ok(playing(&["juego.exe", "discord.exe", "firefox.exe"])),
        })));
        assert_eq!(app.host.target, Some(game.clone()));
        let rows = app.host.audio_rows(&app.settings.audio.excluded_apps);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.checked), "todas marcadas");
        assert_eq!(
            app.host.game_capture(&app.settings.audio.excluded_apps),
            GameCapture::Apps(AppFilter::AllExcept(Vec::new()))
        );
        // Quitar una se recuerda, y elegir otra ventana no toca el audio.
        app.update(Message::Host(Msg::AppAudio(
            "Discord.exe".into(),
            "Discord".into(),
            false,
        )));
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        assert_eq!(app.settings.audio.excluded_apps[0].key, "discord.exe");
        app.update(Message::Host(Msg::Target(TargetOption(window(
            8,
            "Navegador",
            Some("firefox.exe"),
        )))));
        app.update(Message::Host(Msg::Target(TargetOption(game.clone()))));
        let rows = app.host.audio_rows(&app.settings.audio.excluded_apps);
        assert_eq!(
            rows.iter()
                .filter(|row| !row.checked)
                .map(|row| row.key.as_str())
                .collect::<Vec<_>>(),
            ["discord.exe"]
        );
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
                audio: GameCapture::Apps(AppFilter::AllExcept(vec!["discord.exe".into()])),
                voice: app.settings.audio.host_voice,
                guests_muted: false,
            })
        );
    }

    /// Sin audio por aplicación en el equipo (o cuando el sistema elige la ventana), la ventana
    /// manda lo mismo que la pantalla.
    #[test]
    fn the_preview_only_works_while_it_is_on_screen() {
        use crate::ui::app::Message;
        let mut app = app();
        super::shots::host_room(&mut app);
        assert!(app.preview_visible());
        let away = app.update(Message::Navigate(Page::Home));
        assert!(away.contains(&Effect::PreviewVisible(false)), "{away:?}");
        let back = app.update(Message::Navigate(Page::Host));
        assert!(back.contains(&Effect::PreviewVisible(true)), "{back:?}");
        assert_eq!(
            app.update(Message::WindowOccluded(true)),
            [Effect::PreviewVisible(false)]
        );
        assert_eq!(
            app.update(Message::WindowOccluded(false)),
            [Effect::PreviewVisible(true)]
        );
    }

    #[test]
    fn a_window_without_per_app_audio_sends_the_system_mix() {
        let mut app = app();
        app.update(Message::Host(Msg::Mode(ShareMode::Window)));
        // El sistema elige la ventana (Linux): todo lo que suena, salvo lo quitado.
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: Vec::new(),
            targets_error: None,
            apps: Ok(playing(&["discord", "firefox", "juego"])),
        })));
        let excluded = [RememberedApp {
            key: "Discord".into(),
            name: "Discord".into(),
        }];
        assert_eq!(
            app.host.game_capture(&excluded),
            GameCapture::Apps(AppFilter::AllExcept(vec!["discord".into()]))
        );
        // Sin audio por aplicación en el equipo: todo lo que suena.
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: Vec::new(),
            targets_error: None,
            apps: Err("no".into()),
        })));
        assert_eq!(app.host.game_capture(&excluded), GameCapture::System);
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
    fn the_saved_portal_permission_goes_with_the_request_and_is_kept_up_to_date() {
        let mut app = app();
        app.update(Message::Navigate(Page::Host));
        // Lo que `portal.json` tenía al arrancar la aplicación.
        app.host
            .restore_tokens
            .set(ShareKind::Screen, Some("guardado".to_owned()));
        let effects = app.update(Message::Host(Msg::Start));
        let Effect::StartHost(request) = &effects[0] else {
            panic!("se esperaba abrir la sala: {effects:?}");
        };
        assert_eq!(request.share.kind, ShareKind::Screen);
        assert_eq!(
            request.share.restore_token.as_deref(),
            Some("guardado"),
            "sin selector, va el token guardado: el portal no pregunta"
        );
        // Al arrancar, el portal renovó el token: se persiste el nuevo.
        let effects = app.update(Message::Event(Event::ShareRestoreToken(
            ShareKind::Screen,
            Some("renovado".to_owned()),
        )));
        assert!(
            matches!(&effects[..], [Effect::SaveRestoreTokens(tokens)]
                if tokens.get(ShareKind::Screen) == Some("renovado")
                    && tokens.get(ShareKind::App).is_none()),
            "{effects:?}"
        );
        assert_eq!(
            app.host.share_choice().restore_token.as_deref(),
            Some("renovado")
        );
        // El mismo token otra vez no escribe nada.
        assert!(
            app.update(Message::Event(Event::ShareRestoreToken(
                ShareKind::Screen,
                Some("renovado".to_owned()),
            )))
            .is_empty()
        );
        // La ventana tiene su propio token: no toca el de la pantalla.
        let effects = app.update(Message::Event(Event::ShareRestoreToken(
            ShareKind::App,
            Some("ventana".to_owned()),
        )));
        assert!(
            matches!(&effects[..], [Effect::SaveRestoreTokens(tokens)]
                if tokens.get(ShareKind::Screen) == Some("renovado")
                    && tokens.get(ShareKind::App) == Some("ventana")),
            "{effects:?}"
        );
        // El portal rechazó el guardado: se olvida y la próxima petición va sin token (el
        // diálogo saldrá una vez).
        let effects = app.update(Message::Event(Event::ShareRestoreToken(
            ShareKind::Screen,
            None,
        )));
        assert!(
            matches!(&effects[..], [Effect::SaveRestoreTokens(tokens)]
                if tokens.get(ShareKind::Screen).is_none()
                    && tokens.get(ShareKind::App) == Some("ventana")),
            "{effects:?}"
        );
        assert_eq!(app.host.share_choice().restore_token, None);
    }

    #[test]
    fn the_token_of_the_picker_is_saved_and_the_latest_one_wins() {
        let mut app = app();
        app.update(Message::Navigate(Page::Host));
        let monitor = CaptureTarget::Monitor {
            id: 0,
            name: "2560 × 1440".to_owned(),
        };
        let effects = app.update(Message::Event(Event::SharePicked(
            SourceChoice::Screen,
            Ok(Some(PickedShare {
                target: monitor,
                restore_token: Some("elegido".to_owned()),
            })),
        )));
        assert!(
            matches!(&effects[..], [Effect::SaveRestoreTokens(tokens)]
                if tokens.get(ShareKind::Screen) == Some("elegido")),
            "lo elegido en el selector se recuerda para la próxima ejecución: {effects:?}"
        );
        assert_eq!(
            app.host.share_choice().restore_token.as_deref(),
            Some("elegido")
        );
        // Al abrir la sala el portal lo renovó: la próxima sala lleva el renovado, no el del
        // selector (el portal solo acepta el último que dio).
        app.update(Message::Event(Event::ShareRestoreToken(
            ShareKind::Screen,
            Some("renovado".to_owned()),
        )));
        assert_eq!(
            app.host.share_choice().restore_token.as_deref(),
            Some("renovado")
        );
        assert_eq!(
            app.host
                .picked
                .as_ref()
                .and_then(|picked| picked.restore_token.as_deref()),
            Some("elegido"),
            "lo elegido en pantalla no cambia"
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
        assert_eq!(
            effects,
            [Effect::Sound(crate::ui::settings::SoundEvent::MemberJoined)],
            "solo el aviso de que entra alguien"
        );
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

    /// Un amigo pide entrar: su tarjeta con la sala abierta (una por amigo), aviso si Compartir no
    /// está a la vista; «Dejar entrar» lo invita y «Ahora no» se lo dice.
    #[test]
    fn join_requests_are_listed_and_answered() {
        let mut app = app();
        app.screen = Screen::Main;
        app.update(Message::Event(Event::Friends(Ok(Friends {
            friends: vec![friend(4, "Dani", true), friend(5, "Eli", true)],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }))));
        // Sin sala en este equipo: solo el aviso.
        app.update(Message::Event(Event::JoinRequested {
            from_user: 4,
            from_name: "Dani".into(),
        }));
        assert!(app.host.join_requests.is_empty());
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text.contains("Dani pide entrar"))
        );
        app.toasts.clear();
        open_room(&mut app);
        assert_eq!(app.page, Page::Host);
        app.toasts.clear();
        app.update(Message::Event(Event::JoinRequested {
            from_user: 4,
            from_name: "Dani".into(),
        }));
        assert_eq!(app.host.join_requests.len(), 1);
        assert!(app.toasts.is_empty(), "a la vista: sin aviso");
        // Desde otra página sí avisa; repetir sustituye la tarjeta (sigue habiendo una).
        app.page = Page::Home;
        app.now += Duration::from_secs(5);
        app.update(Message::Event(Event::JoinRequested {
            from_user: 4,
            from_name: "Dani".into(),
        }));
        app.update(Message::Event(Event::JoinRequested {
            from_user: 5,
            from_name: "Eli".into(),
        }));
        assert_eq!(app.host.join_requests.len(), 2);
        assert_eq!(app.host.join_requests[0].at, app.now);
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text.contains("Dani pide entrar"))
        );
        app.toasts.clear();
        // Dejar entrar: la invitación de siempre.
        let effects = app.update(Message::Host(Msg::AllowJoin(4)));
        assert_eq!(effects, [Effect::Invite(4)]);
        assert!(app.toasts.iter().any(|t| t.text.contains("Dani")));
        assert_eq!(app.host.join_requests.len(), 1);
        // Ahora no.
        assert_eq!(
            app.update(Message::Host(Msg::DeclineJoin(5))),
            [Effect::DeclineJoin(5)]
        );
        assert!(app.host.join_requests.is_empty());
        // Sin petición no se hace nada.
        assert!(app.update(Message::Host(Msg::AllowJoin(4))).is_empty());
        assert!(app.update(Message::Host(Msg::DeclineJoin(5))).is_empty());
    }

    /// Las peticiones caducan a los [`JOIN_REQUEST_TTL`] (el `Tick` se pide para entonces y, con
    /// la sala a la vista, cada segundo), desaparecen cuando el amigo entra y con la sala cerrada.
    #[test]
    fn join_requests_expire_or_end_when_the_guest_joins() {
        let mut app = app();
        app.screen = Screen::Main;
        open_room(&mut app);
        let start = app.now;
        app.update(Message::Event(Event::JoinRequested {
            from_user: 13,
            from_name: "Dani".into(),
        }));
        assert_eq!(next_tick(&app), Some(start + Duration::from_secs(1)));
        app.now += Duration::from_millis(2_300);
        assert_eq!(next_tick(&app), Some(start + Duration::from_secs(3)));
        app.page = Page::Home;
        assert_eq!(next_tick(&app), Some(start + JOIN_REQUEST_TTL));
        app.now = start + JOIN_REQUEST_TTL - Duration::from_secs(1);
        app.update(Message::Tick);
        assert_eq!(app.host.join_requests.len(), 1, "todavía");
        app.now = start + JOIN_REQUEST_TTL;
        app.update(Message::Tick);
        assert!(app.host.join_requests.is_empty(), "caducó");
        // Se pliega en su sitio y después se olvida.
        assert_eq!(app.host.motion.requests_leaving.len(), 1, "plegándose");
        let exit = app.motion().exit_duration(Token::Panel);
        assert_eq!(next_tick(&app), Some(app.now + exit));
        app.now += exit;
        app.update(Message::Tick);
        assert!(app.host.motion.requests_leaving.is_empty(), "plegada");
        assert_eq!(next_tick(&app), None);
        // Entra quien lo pidió: su tarjeta sobra.
        app.update(Message::Event(Event::JoinRequested {
            from_user: 13,
            from_name: "Dani".into(),
        }));
        app.update(Message::Event(Event::Host(SessionEvent::GuestJoined {
            guest: GuestId(3),
            user: 13,
            name: "Dani".into(),
            fingerprint: "ef".repeat(32),
            safety_number: coroom_transport::crypto::SafetyNumber::between(&[1; 32], &[3; 32]),
            permissions: Permissions::VIEW_ONLY,
        })));
        assert!(app.host.join_requests.is_empty());
        // Al cerrar la sala no queda ninguna.
        app.update(Message::Event(Event::JoinRequested {
            from_user: 14,
            from_name: "Eli".into(),
        }));
        app.update(Message::Event(Event::HostEnded(None)));
        assert!(app.host.join_requests.is_empty());
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
        assert_eq!((video.max_resolution, video.fps), (Resolution::P1080, 60));
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        app.update(Message::Host(Msg::Preset(QualityPreset::MaxQuality)));
        assert_eq!(app.settings.video.fps, 120);
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
            Msg::Chroma444(true),
            Msg::Hdr(true),
        ] {
            app.update(Message::Host(Msg::Preset(QualityPreset::Balanced)));
            assert!(app.update(Message::Host(msg.clone())).is_empty(), "{msg:?}");
            assert_eq!(app.settings.video.preset, QualityPreset::Custom, "{msg:?}");
        }
    }

    /// Con la sala abierta, lo «En vivo» se aplica ya (la resolución máxima y los fps también,
    /// con el efecto que reabre el codificador) y lo «Al abrir la sala» solo se guarda con un
    /// aviso.
    #[test]
    fn with_the_room_open_live_options_apply_and_the_rest_wait() {
        let mut app = app();
        open_room(&mut app);
        app.toasts.clear();
        assert_eq!(
            app.update(Message::Host(Msg::Codec(pick(CodecChoice::Av1, "AV1")))),
            [Effect::SetEncoding]
        );
        assert_eq!(
            app.update(Message::Host(Msg::Fps(30))),
            [Effect::SetEncoding],
            "los fps van en vivo"
        );
        assert_eq!(
            app.update(Message::Host(Msg::Resolution(pick(
                Resolution::P720,
                "720p"
            )))),
            [Effect::SetEncoding],
            "la resolución va en vivo"
        );
        assert!(app.toasts.is_empty(), "lo en vivo no avisa de nada");
        assert!(app.update(Message::Host(Msg::Chroma444(true))).is_empty());
        assert!(
            app.toasts
                .iter()
                .any(|t| t.text == "Guardado para la próxima sala")
        );
        // Un perfil mezcla las dos cosas: la resolución y los fps van ya, juntos en un solo
        // efecto; 4:4:4 espera.
        app.toasts.clear();
        app.update(Message::Host(Msg::Chroma444(false)));
        app.toasts.clear();
        assert_eq!(
            app.update(Message::Host(Msg::Preset(QualityPreset::MaxQuality))),
            [Effect::SetEncoding]
        );
        assert_eq!(app.toasts.len(), 1, "{:?}", app.toasts);
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        // Repetir el valor no toca nada.
        app.toasts.clear();
        assert!(app.update(Message::Host(Msg::Fps(120))).is_empty());
        assert!(app.toasts.is_empty());
    }

    /// Cuando la sala aplicó el cambio, el aviso dice lo que codifica ahora.
    #[test]
    fn the_room_confirms_the_encoding_it_applied() {
        let mut app = app();
        open_room(&mut app);
        app.update(Message::Host(Msg::Resolution(pick(
            Resolution::P1080,
            "1080p",
        ))));
        app.update(Message::Host(Msg::Fps(30)));
        app.toasts.clear();
        app.update(Message::Event(Event::VideoChanged(Ok(()))));
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(
            app.toasts[0].text,
            "Vídeo de la sala cambiado: 1080p · 30 fps · Automático."
        );
    }

    /// Las etiquetas salen de [`Video::when`].
    #[test]
    fn the_when_summary_comes_from_the_settings_model() {
        let summary = when_summary();
        assert!(summary.starts_with("Al momento: "), "{summary}");
        assert!(summary.contains("En la próxima sala: "), "{summary}");
        let mut video = Video::default();
        QualityPreset::MinLatency.apply(&mut video);
        assert_eq!(video_spec(&video), "1080p · 60 fps");
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
        app.update(Message::Host(Msg::Mode(ShareMode::Window)));
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
    fn each_guest_has_a_volume_that_is_remembered_by_name() {
        let mut app = app();
        app.settings.audio.set_voice_volume("Carla", 60);
        open_room(&mut app);
        let room = app.host.room.as_ref().expect("sala");
        assert_eq!(room.volume(GuestId(1)), 100, "Bea, sin tocar");
        assert_eq!(room.volume(GuestId(2)), 60, "Carla, el recordado al llegar");
        // Quien llega con un volumen recordado suena así desde el principio.
        app.settings.audio.set_voice_volume("Dani", 130);
        let effects = app.update(Message::Event(Event::HostGuests(
            vec![status(1, "Bea"), status(2, "Carla"), status(3, "Dani")],
            HostVoice::default(),
        )));
        assert_eq!(
            effects,
            [Effect::SetVoiceGain {
                host: true,
                source: 3,
                gain: 1.3,
            }],
            "solo el nuevo; Carla ya tenía el suyo"
        );
        // Arrastrar cambia el volumen en caliente; soltar lo recuerda por el nombre.
        let effects = app.update(Message::Host(Msg::Volume(GuestId(1), 150)));
        assert_eq!(
            effects,
            [Effect::SetVoiceGain {
                host: true,
                source: 1,
                gain: 1.5,
            }]
        );
        assert_eq!(
            app.host.room.as_ref().expect("sala").volume(GuestId(1)),
            150
        );
        assert_eq!(
            app.settings.audio.voice_volume("Bea"),
            100,
            "aún no se soltó"
        );
        assert!(
            app.update(Message::Host(Msg::VolumeDone(GuestId(1))))
                .is_empty()
        );
        assert_eq!(app.settings.audio.voice_volume("Bea"), 150);
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));
        // Volver al 100 % lo olvida, y quien se va no deja entrada.
        app.update(Message::Host(Msg::Volume(GuestId(1), 100)));
        app.update(Message::Host(Msg::VolumeDone(GuestId(1))));
        assert!(!app.settings.audio.voice_volumes.contains_key("Bea"));
        app.update(Message::Event(Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(2),
            kicked: false,
        })));
        let room = app.host.room.as_ref().expect("sala");
        assert!(!room.volumes.contains_key(&GuestId(2)));
        assert_eq!(room.volume(GuestId(3)), 130);
        // Fuera de rango se acota al máximo.
        app.update(Message::Host(Msg::Volume(GuestId(3), 255)));
        assert_eq!(
            app.host.room.as_ref().expect("sala").volume(GuestId(3)),
            MAX_VOICE_VOLUME
        );
    }

    #[test]
    fn local_mutes_are_remembered_by_name_when_a_guest_comes_back() {
        let mut app = app();
        // Sin los avisos de quien entra y sale: aquí solo cuentan los efectos de la voz.
        app.settings.notifications.volume = 0;
        app.settings.audio.set_voice_muted("Carla", true);
        // Al abrir la sala, lo recordado se aplica a quien llega.
        app.update(Message::Host(Msg::Start));
        app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        app.update(Message::Event(Event::Host(SessionEvent::RoomCreated {
            code: "K7Q2MZ".into(),
            fingerprint: "cd".repeat(32),
        })));
        let effects = app.update(Message::Event(Event::HostGuests(
            vec![status(1, "Bea"), status(2, "Carla")],
            HostVoice::default(),
        )));
        assert_eq!(
            effects,
            [Effect::SetVoiceMuted {
                host: true,
                source: 2,
                muted: true,
            }]
        );
        // Dejar de oír a Bea se aplica y se recuerda por su nombre.
        let effects = app.update(Message::Host(Msg::Hear(GuestId(1), false)));
        assert_eq!(
            effects,
            [Effect::SetVoiceMuted {
                host: true,
                source: 1,
                muted: true,
            }]
        );
        assert!(app.settings.audio.voice_muted("Bea"));
        // Bea se va (su sitio en el audio queda libre) y vuelve con otro id: sigue silenciada.
        let effects = app.update(Message::Event(Event::Host(SessionEvent::GuestLeft {
            guest: GuestId(1),
            kicked: false,
        })));
        assert_eq!(
            effects,
            [Effect::SetVoiceMuted {
                host: true,
                source: 1,
                muted: false,
            }]
        );
        let effects = app.update(Message::Event(Event::HostGuests(
            vec![status(2, "Carla"), status(3, "Bea")],
            HostVoice::default(),
        )));
        assert_eq!(
            effects,
            [Effect::SetVoiceMuted {
                host: true,
                source: 3,
                muted: true,
            }]
        );
        // Volver a oír a Carla lo olvida.
        app.update(Message::Host(Msg::Hear(GuestId(2), true)));
        assert!(!app.settings.audio.voice_muted("Carla"));
    }

    #[test]
    fn the_lobby_stops_once_the_room_is_open() {
        let mut app = app();
        let effects = app.update(Message::Host(Msg::Start));
        assert!(!effects.contains(&Effect::StopLobby), "{effects:?}");
        let started = app.update(Message::Event(Event::HostStarted(
            Ok(VideoFeed::detached()),
        )));
        assert_eq!(started[0], Effect::StopLobby, "{started:?}");
        assert!(
            started.contains(&Effect::PreviewVisible(app.preview_visible())),
            "{started:?}"
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
        app.update(Message::Host(Msg::Mode(ShareMode::Window)));
        let game = window(1, "Juego", Some("juego.exe"));
        let other = window(2, "Otro juego", Some("otro.exe"));
        app.update(Message::Event(Event::ShareOptions(ShareOptions {
            source: SourceChoice::App,
            targets: vec![game.clone(), other.clone()],
            targets_error: None,
            apps: Ok(playing(&["juego.exe", "otro.exe", "discord.exe"])),
        })));
        open_room(&mut app);
        assert!(!app.host.source_changed());

        // Audio: quitar una aplicación se aplica ya a la sala (y se recuerda).
        let effects = app.update(Message::Host(Msg::AppAudio(
            "discord.exe".into(),
            "discord".into(),
            false,
        )));
        assert_eq!(
            effects,
            [Effect::SetAppAudio(GameCapture::Apps(
                AppFilter::AllExcept(vec!["discord.exe".into()])
            ))]
        );
        assert_eq!(app.flush_settings(), Some(Effect::SaveSettings));

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
            GameCapture::Apps(AppFilter::AllExcept(vec!["discord.exe".into()])),
            "el audio va con la ventana nueva"
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
    }

    fn is_open(app: &App, person: Person) -> bool {
        app.host
            .motion
            .people
            .get(&person)
            .is_some_and(Fold::is_open)
    }

    /// El desplegable de una persona se abre y se cierra solo cuando el usuario lo pide: lo que
    /// llega de la sesión (quién habla, la latencia, un mando, los permisos) no lo toca.
    #[test]
    fn a_person_opens_only_when_the_user_asks() {
        let mut app = app();
        app.screen = Screen::Main;
        open_room(&mut app);
        let people = [
            Person::Me,
            Person::Guest(GuestId(1)),
            Person::Guest(GuestId(2)),
        ];
        let changed = |talkers: Vec<u32>| {
            let mut bea = status(1, "Bea");
            bea.rtt_ms = Some(48.0);
            bea.permissions = Permissions {
                keyboard_mouse: true,
                gamepad_slot: Some(1),
                voice: true,
                gamepad_raw: true,
            };
            bea.gamepad = Some(coroom_session::remote::GuestPad {
                name: "Mando de ejemplo".into(),
                vendor: 0x1234,
                product: 0x5678,
                transport: coroom_session::remote::PadTransport::Hid,
                raw: true,
                note: Some("nota".into()),
            });
            Message::Event(Event::HostGuests(
                vec![bea, status(2, "Carla")],
                HostVoice {
                    enabled: true,
                    mic_muted: false,
                    talkers,
                },
            ))
        };
        app.update(changed(vec![1, 2]));
        app.update(Message::Host(Msg::Keyboard(GuestId(2), true)));
        assert!(
            people.iter().all(|person| !is_open(&app, *person)),
            "cerrados"
        );

        app.update(Message::Host(Msg::Expand(Person::Guest(GuestId(1)))));
        assert!(is_open(&app, Person::Guest(GuestId(1))));
        app.update(changed(Vec::new()));
        app.now += Duration::from_secs(5);
        app.update(Message::Tick);
        assert!(is_open(&app, Person::Guest(GuestId(1))), "sigue abierto");
        assert!(!is_open(&app, Person::Me) && !is_open(&app, Person::Guest(GuestId(2))));

        app.update(Message::Host(Msg::Expand(Person::Me)));
        app.update(Message::Host(Msg::Expand(Person::Guest(GuestId(1)))));
        assert!(is_open(&app, Person::Me));
        assert!(
            !is_open(&app, Person::Guest(GuestId(1))),
            "cerrado por el usuario"
        );
        // Se abre con su animación (y la sala pide cuadros mientras tanto).
        let fold = app.host.motion.people[&Person::Me].clone();
        assert!(fold.shown(app.now) < 1.0);
        assert!(animating(&app));
        app.now += app.motion().duration(Token::Panel);
        assert!((fold.shown(app.now) - 1.0).abs() < f32::EPSILON);
    }

    /// «EN VIVO» lleva el tiempo transmitiendo (m:ss y, pasada una hora, h:mm:ss), se pide un
    /// cuadro por segundo para él solo con la sala delante y desaparece al dejar de compartir.
    #[test]
    fn en_vivo_shows_its_time() {
        let mut app = app();
        app.screen = Screen::Main;
        open_room(&mut app);
        assert_eq!(live_label(&app).as_deref(), Some("EN VIVO 0:00"));
        let since = app.host.motion.live_since.expect("en vivo");
        app.now = since + Duration::from_millis(12 * 60_000 + 4_300);
        assert_eq!(live_label(&app).as_deref(), Some("EN VIVO 12:04"));
        settle(&mut app);
        assert_eq!(
            next_tick(&app),
            Some(since + Duration::from_secs(12 * 60 + 5)),
            "el segundo siguiente"
        );
        assert!(!animating(&app), "nada de continuo: un cuadro por segundo");
        app.now = since + Duration::from_secs(3_600 + 2 * 60 + 3);
        assert_eq!(live_label(&app).as_deref(), Some("EN VIVO 1:02:03"));
        app.page = Page::Home;
        assert_eq!(next_tick(&app), None, "sin la sala delante, nada");
        app.page = Page::Host;

        app.update(Message::Host(Msg::StopSharing));
        app.update(Message::Event(Event::SourceSwitched(Ok(()))));
        assert_eq!(live_label(&app), None, "solo voz");
        assert!(app.host.motion.live_ended.is_some(), "el rojo se apaga");
        assert!(animating(&app));
        app.now += app.motion().duration(Token::Screen);
        assert!(!animating(&app));
    }

    /// Copiar el código cambia el icono a una marca y vuelve a los 1,5 s.
    #[test]
    fn the_copied_mark_goes_back() {
        let mut app = app();
        app.screen = Screen::Main;
        open_room(&mut app);
        settle(&mut app);
        app.host.motion.live_since = None;
        app.update(Message::Host(Msg::CopyCode));
        let at = app.host.motion.copied_at.expect("copiado");
        assert_eq!(next_tick(&app), Some(at + COPIED));
        app.now = at + COPIED;
        app.update(Message::Tick);
        assert_eq!(app.host.motion.copied_at, None);
        assert_eq!(next_tick(&app), None);
    }
}
