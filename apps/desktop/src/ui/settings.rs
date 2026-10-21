//! Ajustes de la aplicación, guardados en `COROOM_DATA_DIR/settings.toml` (por defecto, el
//! directorio de datos del usuario: [`crate::remote::default_data_dir`]).
//!
//! El fichero es TOML legible y tolerante: lo que falta toma su valor por defecto y lo que no se
//! entiende se ignora (con un aviso), así una versión más nueva o una edición a mano no dejan la
//! aplicación sin arrancar. Se escribe entero en un fichero temporal y se renombra (nunca queda a
//! medias). No guarda secretos: las credenciales van al llavero ([`crate::account`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use coroom_session::input::gamepad::PadChoice;
use coroom_ui::motion::Motion;
use serde::{Deserialize, Serialize};

use super::tones::Builtin;

/// Nombre del fichero dentro del directorio de datos.
pub const FILE_NAME: &str = "settings.toml";

/// Tema pedido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeChoice {
    /// El del sistema (portal `Settings` o registro de Windows).
    #[default]
    System,
    /// Claro.
    Light,
    /// Oscuro.
    Dark,
}

/// Animaciones pedidas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MotionChoice {
    /// Las que pida el sistema (movimiento reducido y factor de duración).
    #[default]
    System,
    /// Reducidas: fundidos cortos, sin desplazamientos, escalas ni escalonados.
    Reduced,
    /// Completas, aunque el sistema pida menos.
    Full,
}

impl MotionChoice {
    /// El movimiento efectivo con lo que pide el sistema. El factor de duración del sistema se
    /// respeta en los tres casos, salvo con «Completas» si el sistema las quita del todo (factor
    /// 0): entonces van con sus duraciones.
    pub fn apply(self, system: Motion) -> Motion {
        match self {
            Self::System => system,
            Self::Reduced => system.with_reduced(true),
            Self::Full if system.is_off() => Motion::FULL,
            Self::Full => system.with_reduced(false),
        }
    }
}

/// Apariencia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Claro, oscuro o el del sistema.
    pub theme: ThemeChoice,
    /// Acento: `"system"` o una clave de [`coroom_ui::theme::Accent::key`].
    pub accent: String,
    /// Animaciones: las del sistema, reducidas o completas.
    pub motion: MotionChoice,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            accent: "system".to_owned(),
            motion: MotionChoice::System,
        }
    }
}

/// Códec preferido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodecChoice {
    /// El mejor común con los invitados.
    #[default]
    Auto,
    /// AV1.
    Av1,
    /// HEVC.
    Hevc,
    /// H.264.
    H264,
}

/// Codificador del anfitrión.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EncoderChoice {
    /// El primero que funcione, en el orden de preferencia de esta plataforma.
    #[default]
    Auto,
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD AMF.
    Amf,
    /// Intel Quick Sync.
    Qsv,
    /// Vulkan Video.
    Vulkan,
    /// VA-API (Linux).
    Vaapi,
    /// Por software.
    Software,
}

impl EncoderChoice {
    /// Backend pedido (`None`: automático).
    pub fn backend(self) -> Option<coroom_session::codec::Backend> {
        use coroom_session::codec::Backend;
        match self {
            EncoderChoice::Auto => None,
            EncoderChoice::Nvenc => Some(Backend::Nvenc),
            EncoderChoice::Amf => Some(Backend::Amf),
            EncoderChoice::Qsv => Some(Backend::Qsv),
            EncoderChoice::Vulkan => Some(Backend::VulkanVideo),
            EncoderChoice::Vaapi => Some(Backend::Vaapi),
            EncoderChoice::Software => Some(Backend::Software),
        }
    }
}

/// Decodificador del invitado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecoderChoice {
    /// Por GPU si se puede.
    #[default]
    Auto,
    /// Por software (más CPU; para descartar fallos del driver).
    Software,
}

/// Resolución máxima de lo que se comparte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resolution {
    /// 1280 × 720.
    P720,
    /// 1920 × 1080.
    #[default]
    P1080,
    /// 2560 × 1440.
    P1440,
    /// 3840 × 2160.
    P2160,
}

impl Resolution {
    /// Todas, de menor a mayor.
    pub const ALL: [Resolution; 4] = [
        Resolution::P720,
        Resolution::P1080,
        Resolution::P1440,
        Resolution::P2160,
    ];

    /// Ancho y alto.
    pub const fn size(self) -> (u32, u32) {
        match self {
            Resolution::P720 => (1280, 720),
            Resolution::P1080 => (1920, 1080),
            Resolution::P1440 => (2560, 1440),
            Resolution::P2160 => (3840, 2160),
        }
    }
}

/// Calidad elegida al abrir la sala: tres perfiles cerrados y «Personalizado» cuando se toca
/// algo en Avanzado ([`Video::preset`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QualityPreset {
    /// 1080p a 60 fps con tope de 10 Mbit/s.
    MinLatency,
    /// Resolución nativa (hasta 4K) a 60 fps con tope de 20 Mbit/s.
    #[default]
    Balanced,
    /// Resolución nativa (hasta 4K) a 120 fps sin tope y 4:4:4.
    MaxQuality,
    /// Lo que haya en Avanzado.
    Custom,
}

impl QualityPreset {
    /// Los tres perfiles cerrados, en orden.
    pub const PRESETS: [QualityPreset; 3] = [
        QualityPreset::MinLatency,
        QualityPreset::Balanced,
        QualityPreset::MaxQuality,
    ];

    /// Aplica el perfil a las opciones de vídeo (`Custom` no toca nada).
    pub fn apply(self, video: &mut Video) {
        match self {
            QualityPreset::MinLatency => {
                video.max_resolution = Resolution::P1080;
                video.fps = 60;
                video.chroma_444 = false;
            }
            QualityPreset::Balanced => {
                video.max_resolution = Resolution::P2160;
                video.fps = 60;
                video.chroma_444 = false;
            }
            QualityPreset::MaxQuality => {
                video.max_resolution = Resolution::P2160;
                video.fps = 120;
                video.chroma_444 = true;
            }
            QualityPreset::Custom => {}
        }
        video.preset = self;
    }

    /// El perfil que corresponde a estas opciones (`Custom` si no coincide ninguno).
    pub fn matching(video: &Video) -> QualityPreset {
        Self::PRESETS
            .into_iter()
            .find(|preset| {
                let mut probe = video.clone();
                preset.apply(&mut probe);
                probe.max_resolution == video.max_resolution
                    && probe.fps == video.fps
                    && probe.chroma_444 == video.chroma_444
            })
            .unwrap_or(QualityPreset::Custom)
    }
}

/// Cuándo surte efecto un ajuste de la sala: ya, con la sala abierta, o en la próxima.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Se aplica en vivo (etiqueta «En vivo»).
    Live,
    /// Se aplica al abrir la sala (etiqueta «Al abrir la sala»).
    NextRoom,
}

/// Cada opción de vídeo de Avanzado, para saber cuándo se aplica ([`Video::when`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoOption {
    /// Resolución máxima.
    Resolution,
    /// Fotogramas por segundo.
    Fps,
    /// Códec (y codificador).
    Codec,
    /// Croma 4:4:4.
    Chroma444,
    /// HDR10.
    Hdr,
}

/// Vídeo del anfitrión.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Video {
    /// Códec preferido.
    pub codec: CodecChoice,
    /// Fotogramas por segundo (30, 60 o 120).
    pub fps: u32,
    /// Resolución máxima.
    pub max_resolution: Resolution,
    /// Croma 4:4:4 (texto nítido) cuando el codificador y los invitados lo admitan; como
    /// invitado, aceptarlo.
    pub chroma_444: bool,
    /// HDR10 cuando la captura, el codificador y los invitados lo admitan; como invitado,
    /// aceptarlo, y la ventana en HDR si la pantalla lo expone.
    pub hdr: bool,
    /// Codificador (avanzado).
    pub encoder: EncoderChoice,
    /// Decodificador como invitado (avanzado).
    pub decoder: DecoderChoice,
    /// Perfil de calidad elegido en Compartir.
    pub preset: QualityPreset,
    /// Como invitado, sincronizar la presentación con el refresco de la pantalla (sin desgarros,
    /// con hasta un refresco más de latencia). Desactivado: la presentación de menor latencia.
    pub vsync: bool,
}

impl Default for Video {
    fn default() -> Self {
        Self {
            codec: CodecChoice::Auto,
            fps: 60,
            max_resolution: Resolution::P2160,
            chroma_444: false,
            hdr: false,
            encoder: EncoderChoice::Auto,
            decoder: DecoderChoice::Auto,
            preset: QualityPreset::Balanced,
            vsync: false,
        }
    }
}

impl Video {
    /// Cuándo surte efecto cada opción: el códec, la resolución máxima y los fps se cambian con
    /// la sala abierta (se reabre el codificador con un intra); 4:4:4 y HDR, al abrir la próxima (cambian cómo se pide la captura, en
    /// GPU o en CPU, y eso solo se decide al arrancarla).
    pub fn when(option: VideoOption) -> When {
        match option {
            VideoOption::Codec | VideoOption::Resolution | VideoOption::Fps => When::Live,
            VideoOption::Chroma444 | VideoOption::Hdr => When::NextRoom,
        }
    }
}

/// Chat y avisos dentro de la sala.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Room {
    /// Los invitados entran con el micrófono cerrado (el permiso de voz se da después).
    pub guests_muted: bool,
    /// Ajustes de antes de [`Notifications`]: si sonaba el chat. Solo se lee, para pasarlo a
    /// [`Notifications::chat_message`] ([`Settings::sanitized`]); ya no se escribe.
    #[serde(rename = "chat_sound", skip_serializing)]
    pub legacy_chat_sound: Option<bool>,
    /// Como [`Room::legacy_chat_sound`], el fichero del tono del chat.
    #[serde(rename = "chat_sound_file", skip_serializing)]
    pub legacy_chat_sound_file: Option<PathBuf>,
    /// Notificación del escritorio con la ventana sin foco.
    pub chat_notifications: bool,
    /// Burbuja de 5 s sobre el vídeo con el panel cerrado.
    pub chat_bubbles: bool,
    /// Como invitado, quién está hablando arriba a la izquierda del vídeo.
    pub talking_overlay: bool,
}

impl Default for Room {
    fn default() -> Self {
        Self {
            guests_muted: false,
            legacy_chat_sound: None,
            legacy_chat_sound_file: None,
            chat_notifications: true,
            chat_bubbles: true,
            talking_overlay: true,
        }
    }
}

/// Cada aviso con sonido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SoundEvent {
    /// Entra alguien en la sala (todos los de la sala; uno mismo al entrar, no).
    MemberJoined,
    /// Sale alguien de la sala, o el anfitrión la cierra (todos los de la sala).
    MemberLeft,
    /// Mensaje nuevo del chat de la sala, con el chat cerrado o la ventana sin foco.
    ChatMessage,
    /// Alguien pide entrar en tu sala («Pedir entrar»; solo el anfitrión).
    JoinRequest,
    /// Te invitan a una sala, o el anfitrión te ofrece quedarte con la suya.
    Invitation,
    /// Te dejan entrar en la sala en la que pediste entrar.
    Admitted,
    /// Te pasaron la sala: ahora eres el anfitrión.
    BecameHost,
    /// Alguien te pide amistad.
    FriendRequest,
}

impl SoundEvent {
    /// Todos, en el orden de Ajustes.
    pub const ALL: [SoundEvent; 8] = [
        SoundEvent::MemberJoined,
        SoundEvent::MemberLeft,
        SoundEvent::ChatMessage,
        SoundEvent::JoinRequest,
        SoundEvent::Invitation,
        SoundEvent::Admitted,
        SoundEvent::BecameHost,
        SoundEvent::FriendRequest,
    ];

    /// El tono propio que suena si no se elige otro.
    pub const fn default_tone(self) -> Builtin {
        match self {
            SoundEvent::MemberJoined => Builtin::Rise,
            SoundEvent::MemberLeft => Builtin::Fall,
            SoundEvent::ChatMessage => Builtin::Chime,
            SoundEvent::JoinRequest => Builtin::Knock,
            SoundEvent::Invitation => Builtin::Bells,
            SoundEvent::Admitted => Builtin::Arpeggio,
            SoundEvent::BecameHost => Builtin::Chord,
            SoundEvent::FriendRequest => Builtin::Bubble,
        }
    }

    /// Prioridad al juntar avisos seguidos (`ui::notify`): uno no suena si en el último segundo
    /// ya sonó otro de su prioridad o más. Lo que pide una respuesta, arriba.
    pub const fn priority(self) -> u8 {
        match self {
            SoundEvent::JoinRequest | SoundEvent::BecameHost => 3,
            SoundEvent::Invitation | SoundEvent::Admitted => 2,
            SoundEvent::FriendRequest => 1,
            SoundEvent::MemberJoined | SoundEvent::MemberLeft | SoundEvent::ChatMessage => 0,
        }
    }
}

/// Qué suena en un aviso: nada, un fichero propio o uno de los tonos de coroom. En
/// `settings.toml`, `"off"`, `"file"` o la clave del tono (`"rise"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum SoundChoice {
    /// Sin sonido.
    Off,
    /// El fichero de [`EventSound::file`].
    File,
    /// Un tono propio.
    Tone(Builtin),
}

impl From<SoundChoice> for String {
    fn from(choice: SoundChoice) -> String {
        match choice {
            SoundChoice::Off => "off".to_owned(),
            SoundChoice::File => "file".to_owned(),
            SoundChoice::Tone(tone) => tone.key().to_owned(),
        }
    }
}

impl TryFrom<String> for SoundChoice {
    type Error = String;

    fn try_from(key: String) -> Result<Self, String> {
        match key.as_str() {
            "off" => Ok(SoundChoice::Off),
            "file" => Ok(SoundChoice::File),
            other => Builtin::from_key(other)
                .map(SoundChoice::Tone)
                .ok_or_else(|| format!("tono desconocido: {other}")),
        }
    }
}

/// El sonido de un aviso. Se lee con [`Notifications`] ([`EventSoundFile`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventSound {
    /// Lo que suena.
    pub tone: SoundChoice,
    /// El fichero propio (se recuerda aunque se elija otro tono, para volver a él). En Windows
    /// solo WAV.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

impl EventSound {
    /// Un tono propio, sin fichero.
    pub const fn tone(tone: Builtin) -> Self {
        Self {
            tone: SoundChoice::Tone(tone),
            file: None,
        }
    }

    /// Si suena algo.
    pub fn is_on(&self) -> bool {
        match self.tone {
            SoundChoice::Off => false,
            SoundChoice::File => self.file.is_some(),
            SoundChoice::Tone(_) => true,
        }
    }
}

/// Volumen de los avisos por defecto (porcentaje).
pub const DEFAULT_NOTIFICATION_VOLUME: u8 = 80;

/// Volumen de partida de los sonidos de la interfaz: la mitad (−12 dB), por debajo de los avisos.
pub const DEFAULT_INTERFACE_VOLUME: u8 = 50;

/// Avisos con sonido: el volumen de todos y qué suena en cada uno. Se lee con
/// [`NotificationsFile`]: un aviso que no se entiende vuelve a su tono sin tirar el resto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "NotificationsFile")]
pub struct Notifications {
    /// Volumen de los avisos, en porcentaje (0-100), aparte del de la sala.
    pub volume: u8,
    /// Entra alguien en la sala.
    pub member_joined: EventSound,
    /// Sale alguien de la sala.
    pub member_left: EventSound,
    /// Mensaje del chat.
    pub chat_message: EventSound,
    /// Alguien pide entrar (anfitrión).
    pub join_request: EventSound,
    /// Te invitan.
    pub invitation: EventSound,
    /// Te dejan entrar.
    pub admitted: EventSound,
    /// Ahora eres el anfitrión.
    pub became_host: EventSound,
    /// Solicitud de amistad.
    pub friend_request: EventSound,
    /// Sonidos de la interfaz (al pulsar, abrir paneles, confirmar), aparte de los avisos.
    /// Encendidos de partida.
    pub interface_sounds: bool,
    /// Volumen de los sonidos de la interfaz, en porcentaje (0-100), aparte del de los avisos.
    pub interface_volume: u8,
}

impl Default for Notifications {
    fn default() -> Self {
        let tone = |event: SoundEvent| EventSound::tone(event.default_tone());
        Self {
            volume: DEFAULT_NOTIFICATION_VOLUME,
            member_joined: tone(SoundEvent::MemberJoined),
            member_left: tone(SoundEvent::MemberLeft),
            chat_message: tone(SoundEvent::ChatMessage),
            join_request: tone(SoundEvent::JoinRequest),
            invitation: tone(SoundEvent::Invitation),
            admitted: tone(SoundEvent::Admitted),
            became_host: tone(SoundEvent::BecameHost),
            friend_request: tone(SoundEvent::FriendRequest),
            interface_sounds: true,
            interface_volume: DEFAULT_INTERFACE_VOLUME,
        }
    }
}

/// [`Notifications`] tal como viene en `settings.toml`, sin fiarse de nada: un valor que no se
/// entiende (un tono que no existe, de otra versión o de una edición a mano; un aviso sin `tone`;
/// un volumen que no es un número) deja el de por defecto de ese aviso, y el resto de los ajustes
/// se queda como estaba.
#[derive(Deserialize, Default)]
#[serde(default)]
struct NotificationsFile {
    volume: Option<toml::Value>,
    member_joined: EventSoundFile,
    member_left: EventSoundFile,
    chat_message: EventSoundFile,
    join_request: EventSoundFile,
    invitation: EventSoundFile,
    admitted: EventSoundFile,
    became_host: EventSoundFile,
    friend_request: EventSoundFile,
    interface_sounds: Option<toml::Value>,
    interface_volume: Option<toml::Value>,
}

/// [`EventSound`] tal como viene en `settings.toml` (ver [`NotificationsFile`]).
#[derive(Deserialize, Default)]
#[serde(default)]
struct EventSoundFile {
    tone: Option<toml::Value>,
    file: Option<toml::Value>,
}

impl EventSoundFile {
    /// El sonido de `event`: lo que se entienda, y su tono por defecto si no.
    fn into_sound(self, event: SoundEvent) -> EventSound {
        let tone = self
            .tone
            .and_then(|tone| SoundChoice::try_from(tone.as_str()?.to_owned()).ok())
            .unwrap_or(SoundChoice::Tone(event.default_tone()));
        let file = self.file.and_then(|file| file.as_str().map(PathBuf::from));
        EventSound { tone, file }
    }
}

impl From<NotificationsFile> for Notifications {
    fn from(file: NotificationsFile) -> Self {
        let volume = file
            .volume
            .and_then(|volume| volume.as_integer())
            .map_or(DEFAULT_NOTIFICATION_VOLUME, |volume| {
                volume.clamp(0, 100) as u8
            });
        Self {
            volume,
            member_joined: file.member_joined.into_sound(SoundEvent::MemberJoined),
            member_left: file.member_left.into_sound(SoundEvent::MemberLeft),
            chat_message: file.chat_message.into_sound(SoundEvent::ChatMessage),
            join_request: file.join_request.into_sound(SoundEvent::JoinRequest),
            invitation: file.invitation.into_sound(SoundEvent::Invitation),
            admitted: file.admitted.into_sound(SoundEvent::Admitted),
            became_host: file.became_host.into_sound(SoundEvent::BecameHost),
            friend_request: file.friend_request.into_sound(SoundEvent::FriendRequest),
            interface_sounds: file
                .interface_sounds
                .and_then(|on| on.as_bool())
                .unwrap_or(true),
            interface_volume: file
                .interface_volume
                .and_then(|volume| volume.as_integer())
                .map_or(DEFAULT_INTERFACE_VOLUME, |volume| {
                    volume.clamp(0, 100) as u8
                }),
        }
    }
}

impl Notifications {
    /// El sonido de `event`.
    pub fn sound(&self, event: SoundEvent) -> &EventSound {
        match event {
            SoundEvent::MemberJoined => &self.member_joined,
            SoundEvent::MemberLeft => &self.member_left,
            SoundEvent::ChatMessage => &self.chat_message,
            SoundEvent::JoinRequest => &self.join_request,
            SoundEvent::Invitation => &self.invitation,
            SoundEvent::Admitted => &self.admitted,
            SoundEvent::BecameHost => &self.became_host,
            SoundEvent::FriendRequest => &self.friend_request,
        }
    }

    /// El sonido de `event`, para cambiarlo.
    pub fn sound_mut(&mut self, event: SoundEvent) -> &mut EventSound {
        match event {
            SoundEvent::MemberJoined => &mut self.member_joined,
            SoundEvent::MemberLeft => &mut self.member_left,
            SoundEvent::ChatMessage => &mut self.chat_message,
            SoundEvent::JoinRequest => &mut self.join_request,
            SoundEvent::Invitation => &mut self.invitation,
            SoundEvent::Admitted => &mut self.admitted,
            SoundEvent::BecameHost => &mut self.became_host,
            SoundEvent::FriendRequest => &mut self.friend_request,
        }
    }

    /// Ganancia lineal del volumen: el cuadrado del porcentaje, que se oye más parejo a lo largo
    /// del deslizador que una recta (80 % ≈ −4 dB, 50 % ≈ −12 dB).
    pub fn gain(&self) -> f32 {
        let fraction = f32::from(self.volume.min(100)) / 100.0;
        fraction * fraction
    }

    /// Ganancia lineal de los sonidos de la interfaz (como [`Notifications::gain`]); cero si están
    /// apagados.
    pub fn interface_gain(&self) -> f32 {
        if !self.interface_sounds {
            return 0.0;
        }
        let fraction = f32::from(self.interface_volume.min(100)) / 100.0;
        fraction * fraction
    }
}

/// Modo de la voz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceMode {
    /// Se abre al hablar.
    #[default]
    Activity,
    /// Pulsar para hablar.
    PushToTalk,
}

/// Audio y voz.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Hablar por la sala con el micrófono.
    pub voice: bool,
    /// Micrófono por nombre (`None`: el del sistema).
    pub microphone: Option<String>,
    /// Salida por nombre (`None`: la del sistema).
    pub speaker: Option<String>,
    /// Activación por voz o pulsar para hablar.
    pub voice_mode: VoiceMode,
    /// Tecla de pulsar para hablar (nombres de `coroom_session::input::keymap`).
    pub push_to_talk_key: String,
    /// Supresión de ruido (WebRTC, Linux).
    pub noise_suppression: bool,
    /// Aplicaciones que se quitan siempre del audio al transmitir la pantalla.
    pub excluded_apps: Vec<RememberedApp>,
    /// Como anfitrión, compartir el micrófono con la sala (se aplica al abrirla). [`Audio::voice`]
    /// es lo mismo como invitado.
    pub host_voice: bool,
    /// Volumen de la voz de cada participante en este equipo, por su nombre de cuenta, en
    /// porcentaje (0-200); solo los distintos de 100. Se aplica cuando esa persona aparece en la
    /// sala, se comparta o se esté de invitado.
    #[serde(default)]
    pub voice_volumes: BTreeMap<String, u8>,
    /// Participantes cuya voz no se oye en este equipo, por su nombre de cuenta (como
    /// [`Audio::voice_volumes`]): se vuelven a silenciar al aparecer en la sala, también al
    /// reconectar o al pasar a otra sala.
    #[serde(default)]
    pub voice_muted: BTreeSet<String>,
    /// Volumen del audio del juego como invitado, en porcentaje (0-200; 100 lo normal), aparte
    /// del de cada voz y de silenciarlo. Se aplica al entrar en una sala.
    pub game_volume: u8,
    /// Volumen de la voz propia que se envía (lo alto que te oyen los demás), en porcentaje
    /// (0-200; 100 lo normal), se comparta o se esté de invitado.
    pub mic_volume: u8,
}

/// Volumen normal de una voz (porcentaje).
pub const DEFAULT_VOICE_VOLUME: u8 = 100;

/// Volumen máximo de una voz (porcentaje; el doble, [`coroom_session::remote::audio::MAX_VOICE_GAIN`]).
pub const MAX_VOICE_VOLUME: u8 = 200;

/// Ganancia lineal de un volumen en porcentaje.
pub fn voice_gain(percent: u8) -> f32 {
    f32::from(percent.min(MAX_VOICE_VOLUME)) / 100.0
}

impl Audio {
    /// Volumen recordado para la voz de `name` ([`DEFAULT_VOICE_VOLUME`] si no se tocó).
    pub fn voice_volume(&self, name: &str) -> u8 {
        self.voice_volumes
            .get(name)
            .copied()
            .unwrap_or(DEFAULT_VOICE_VOLUME)
    }

    /// Recuerda el volumen de la voz de `name`; el normal se olvida (no ocupa sitio).
    pub fn set_voice_volume(&mut self, name: &str, percent: u8) {
        let percent = percent.min(MAX_VOICE_VOLUME);
        if percent == DEFAULT_VOICE_VOLUME {
            self.voice_volumes.remove(name);
        } else {
            self.voice_volumes.insert(name.to_owned(), percent);
        }
    }

    /// Si la voz de `name` está silenciada en este equipo.
    pub fn voice_muted(&self, name: &str) -> bool {
        self.voice_muted.contains(name)
    }

    /// Recuerda (o olvida) que la voz de `name` no se oye en este equipo.
    pub fn set_voice_muted(&mut self, name: &str, muted: bool) {
        if muted {
            self.voice_muted.insert(name.to_owned());
        } else {
            self.voice_muted.remove(name);
        }
    }
}

/// Aplicación recordada en la lista de audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedApp {
    /// Clave (`coroom_session::audio::apps::PlayingApp::key`).
    pub key: String,
    /// Nombre para mostrar.
    pub name: String,
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            voice: true,
            microphone: None,
            speaker: None,
            voice_mode: VoiceMode::Activity,
            push_to_talk_key: "F12".to_owned(),
            noise_suppression: true,
            excluded_apps: Vec::new(),
            host_voice: true,
            voice_volumes: BTreeMap::new(),
            voice_muted: BTreeSet::new(),
            game_volume: DEFAULT_VOICE_VOLUME,
            mic_volume: DEFAULT_VOICE_VOLUME,
        }
    }
}

/// Atajos del invitado (siempre con `Ctrl+Alt`): la letra de cada uno.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    /// Inmersivo / escritorio.
    pub immersive: char,
    /// Soltar el teclado y el ratón (hasta el próximo clic en el vídeo).
    pub release: char,
    /// Panel de conexión (estadísticas).
    pub stats: char,
    /// Chat de la sala.
    pub chat: char,
    /// Salir de la sala.
    pub leave: char,
    /// Silenciar el micrófono.
    pub mic: char,
    /// Silenciar el juego.
    pub game: char,
    /// Panel de mandos.
    pub pads: char,
    /// Apagar o encender el vídeo.
    pub video: char,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            immersive: 'G',
            release: 'R',
            stats: 'S',
            chat: 'C',
            leave: 'Q',
            mic: 'M',
            game: 'V',
            pads: 'J',
            video: 'I',
        }
    }
}

impl Shortcuts {
    /// Las nueve letras, en orden (el de [`Shortcuts::letter_mut`]).
    pub fn letters(&self) -> [char; 9] {
        [
            self.immersive,
            self.release,
            self.stats,
            self.chat,
            self.leave,
            self.mic,
            self.game,
            self.pads,
            self.video,
        ]
    }

    /// La letra del atajo `index` de [`Shortcuts::letters`] (más allá del último, el último).
    pub fn letter_mut(&mut self, index: usize) -> &mut char {
        match index {
            0 => &mut self.immersive,
            1 => &mut self.release,
            2 => &mut self.stats,
            3 => &mut self.chat,
            4 => &mut self.leave,
            5 => &mut self.mic,
            6 => &mut self.game,
            7 => &mut self.pads,
            _ => &mut self.video,
        }
    }

    /// Si no hay letras repetidas y todas son letras ASCII.
    pub fn is_valid(&self) -> bool {
        let letters = self.letters();
        letters.iter().all(char::is_ascii_alphabetic)
            && letters
                .iter()
                .enumerate()
                .all(|(i, a)| letters[i + 1..].iter().all(|b| !a.eq_ignore_ascii_case(b)))
    }

    /// Ajustes de antes de un atajo nuevo (soltar el teclado y el ratón, apagar el vídeo): si
    /// su letra de fábrica ya la usa otro atajo elegido, toma la primera libre en lugar de
    /// perder los demás.
    fn free_new(&mut self) {
        // Índices en `letters`: soltar y el vídeo.
        for index in [1, 8] {
            let taken = |letter: char, shortcuts: &Self| {
                shortcuts.letters()[..]
                    .iter()
                    .enumerate()
                    .any(|(other, used)| other != index && used.eq_ignore_ascii_case(&letter))
            };
            if !taken(self.letters()[index], self) {
                continue;
            }
            let default = Self::default().letters()[index];
            if let Some(letter) = std::iter::once(default)
                .chain('A'..='Z')
                .find(|letter| !taken(*letter, self))
            {
                *self.letter_mut(index) = letter;
            }
        }
    }
}

/// Entrada.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Input {
    /// Atajos editables.
    pub shortcuts: Shortcuts,
    /// Ranura de mando que se ofrece primero al dar mando a un invitado (`0`: la primera libre).
    pub preferred_slot: u8,
    /// Qué mando envía este equipo como invitado.
    pub pad: PadChoice,
}

/// Red.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    /// Usar siempre el relay del servidor.
    pub force_relay: bool,
    /// Puerto UDP local (`0`: cualquiera).
    pub local_port: u16,
    /// Pedir al router que abra el puerto UDP (UPnP, NAT-PMP o PCP) para la conexión directa.
    pub port_mapping: bool,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            force_relay: false,
            local_port: 0,
            port_mapping: true,
        }
    }
}

/// Aviso de versión nueva ([`crate::update`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Updates {
    /// Comprobar si hay una versión nueva en las publicaciones del proyecto.
    pub check: bool,
    /// Cada cuántas horas (24 o 168).
    pub interval_hours: u32,
    /// Última comprobación, en segundos desde 1970.
    pub last_check: Option<u64>,
    /// Descargar e instalar sola la versión nueva (si este paquete puede); apagado, solo se avisa.
    pub auto: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check: true,
            interval_hours: 24,
            last_check: None,
            auto: true,
        }
    }
}

/// Un servidor conocido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEntry {
    /// URL (`https://host[:puerto]`).
    pub url: String,
    /// Huella del certificado autofirmado en el que se confió, si es el caso.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Puerto TCP del respaldo si no es el de la URL.
    #[serde(default)]
    pub tcp_port: Option<u16>,
    /// Último usuario con el que se entró.
    #[serde(default)]
    pub username: Option<String>,
}

/// Un par visto en una sala: su número de seguridad, para compararlo después.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownPeer {
    /// Nombre visible.
    pub name: String,
    /// Huella de su identidad.
    pub fingerprint: String,
    /// Número de seguridad entre los dos.
    pub safety_number: String,
}

/// Todos los ajustes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Apariencia.
    pub appearance: Appearance,
    /// Vídeo.
    pub video: Video,
    /// Audio y voz.
    pub audio: Audio,
    /// Entrada.
    pub input: Input,
    /// Red.
    pub network: Network,
    /// Chat y avisos de la sala.
    pub room: Room,
    /// Avisos con sonido.
    pub notifications: Notifications,
    /// Aviso de versión nueva.
    pub updates: Updates,
    /// Servidores conocidos (el primero, el último usado).
    pub servers: Vec<ServerEntry>,
    /// Pares vistos en las salas.
    pub peers: Vec<KnownPeer>,
}

/// Valores admitidos de fps.
pub const FPS_CHOICES: [u32; 3] = [30, 60, 120];

impl Settings {
    /// Corrige valores fuera de rango (por ejemplo, de una edición a mano).
    pub fn sanitized(mut self) -> Self {
        if !FPS_CHOICES.contains(&self.video.fps) {
            self.video.fps = Video::default().fps;
        }
        if self.video.preset != QualityPreset::Custom
            && QualityPreset::matching(&self.video) != self.video.preset
        {
            // El perfil guardado y las opciones no cuadran (ajustes de otra versión): manda lo
            // que hay en las opciones.
            self.video.preset = QualityPreset::matching(&self.video);
        }
        if !self.input.shortcuts.is_valid() {
            self.input.shortcuts.free_new();
        }
        if !self.input.shortcuts.is_valid() {
            self.input.shortcuts = Shortcuts::default();
        }
        self.input.preferred_slot = self.input.preferred_slot.min(8);
        if coroom_session::input::keymap::keycode_from_name(&self.audio.push_to_talk_key).is_none()
        {
            self.audio.push_to_talk_key = Audio::default().push_to_talk_key;
        }
        for volume in self.audio.voice_volumes.values_mut() {
            *volume = (*volume).min(MAX_VOICE_VOLUME);
        }
        self.audio
            .voice_volumes
            .retain(|_, volume| *volume != DEFAULT_VOICE_VOLUME);
        self.audio.game_volume = self.audio.game_volume.min(MAX_VOICE_VOLUME);
        self.migrate_chat_sound();
        self.notifications.volume = self.notifications.volume.min(100);
        for event in SoundEvent::ALL {
            let sound = self.notifications.sound_mut(event);
            if sound.tone == SoundChoice::File && sound.file.is_none() {
                sound.tone = SoundChoice::Tone(event.default_tone());
            }
        }
        self.audio.mic_volume = self.audio.mic_volume.min(MAX_VOICE_VOLUME);
        let mut seen = std::collections::BTreeSet::new();
        self.servers
            .retain(|server| seen.insert(server.url.clone()));
        self.peers.truncate(64);
        if !crate::update::INTERVAL_CHOICES.contains(&self.updates.interval_hours) {
            self.updates.interval_hours = Updates::default().interval_hours;
        }
        self
    }

    /// Pasa el sonido del chat de los ajustes de antes (`[room] chat_sound` y
    /// `chat_sound_file`) a [`Notifications::chat_message`]: apagado sigue apagado, un fichero
    /// propio sigue sonando y, si no, la campanilla de siempre.
    fn migrate_chat_sound(&mut self) {
        let on = self.room.legacy_chat_sound.take();
        let file = self.room.legacy_chat_sound_file.take();
        if on.is_none() && file.is_none() {
            return;
        }
        let chat = &mut self.notifications.chat_message;
        if file.is_some() {
            chat.file = file;
        }
        chat.tone = match on {
            Some(false) => SoundChoice::Off,
            _ if chat.file.is_some() => SoundChoice::File,
            _ => SoundChoice::Tone(SoundEvent::ChatMessage.default_tone()),
        };
    }

    /// Lee los ajustes de `path`; si no existe, los de por defecto. Si está dañado, los de por
    /// defecto y el motivo (para avisar sin dejar de arrancar).
    pub fn load(path: &Path) -> (Settings, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Settings>(&text) {
                Ok(settings) => (settings.sanitized(), None),
                Err(err) => (
                    Settings::default(),
                    Some(t!(
                        "settings-no-se-pudieron-leer-los-ajustes",
                        display = path.display(),
                        message = err.message()
                    )),
                ),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Settings::default(), None),
            Err(err) => (
                Settings::default(),
                Some(t!(
                    "settings-no-se-pudieron-leer-los-ajustes-2",
                    display = path.display(),
                    err = err
                )),
            ),
        }
    }

    /// Guarda en `path` (en un temporal que se renombra).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self)
            .map_err(|err| t!("settings-no-se-pudieron-preparar-los-ajustes", err = err))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| {
                t!(
                    "settings-no-se-pudo-crear-err",
                    display = dir.display(),
                    err = err
                )
            })?;
        }
        let temporary = temporary_path(path);
        std::fs::write(&temporary, text)
            .map_err(|err| t!("settings-no-se-pudieron-guardar-los-ajustes", err = err))?;
        std::fs::rename(&temporary, path)
            .map_err(|err| t!("settings-no-se-pudieron-guardar-los-ajustes", err = err))
    }

    /// Recuerda `server` como el último usado.
    pub fn remember_server(&mut self, server: ServerEntry) {
        self.servers.retain(|known| known.url != server.url);
        self.servers.insert(0, server);
        self.servers.truncate(12);
    }

    /// Olvida un servidor.
    pub fn forget_server(&mut self, url: &str) {
        self.servers.retain(|known| known.url != url);
    }

    /// Recuerda el número de seguridad de un par (sustituye el anterior del mismo par).
    pub fn remember_peer(&mut self, peer: KnownPeer) {
        self.peers
            .retain(|known| known.fingerprint != peer.fingerprint);
        self.peers.insert(0, peer);
        self.peers.truncate(64);
    }
}

/// Temporal en el que se escribe antes de renombrar a `path`, junto a él y propio de este proceso:
/// dos coroom con el mismo directorio de datos no se pisan el temporal (dentro del proceso, los
/// guardados ya van de uno en uno).
fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension(format!("toml.{}.tmp", std::process::id()))
}

/// Directorio de datos: `COROOM_DATA_DIR` si está definida, si no el del usuario.
pub fn data_dir() -> PathBuf {
    crate::remote::default_data_dir()
}

/// El directorio de datos si se pidió uno con `COROOM_DATA_DIR`: entonces la identidad y las
/// credenciales también van a ficheros de ese directorio (un perfil aparte, sin el llavero), como
/// con `--data-dir` en la línea de órdenes.
pub fn explicit_data_dir() -> Option<PathBuf> {
    std::env::var_os("COROOM_DATA_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// Ruta de los ajustes en `data_dir`.
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_toml() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = path_in(dir.path());
        let (fresh, warning) = Settings::load(&path);
        assert_eq!(fresh, Settings::default());
        assert_eq!(warning, None, "sin fichero no hay aviso");

        let mut settings = Settings::default();
        settings.appearance.theme = ThemeChoice::Dark;
        settings.appearance.accent = "teal".into();
        settings.appearance.motion = MotionChoice::Reduced;
        settings.video.fps = 120;
        settings.video.codec = CodecChoice::Av1;
        settings.video.max_resolution = Resolution::P1440;
        settings.audio.voice_mode = VoiceMode::PushToTalk;
        settings.audio.push_to_talk_key = "CapsLock".into();
        settings.network.force_relay = true;
        settings.network.local_port = 47_000;
        settings.notifications.chat_message = EventSound {
            tone: SoundChoice::File,
            file: Some(PathBuf::from("/home/ana/sonidos/tono.ogg")),
        };
        settings.notifications.member_left.tone = SoundChoice::Off;
        settings.notifications.join_request.tone = SoundChoice::Tone(Builtin::Ping);
        settings.notifications.volume = 55;
        settings.input.shortcuts.immersive = 'K';
        settings.video.chroma_444 = true;
        settings.video.hdr = true;
        // Tocar Avanzado deja el perfil en «Personalizado» (al cargar se comprueba que cuadre).
        settings.video.preset = QualityPreset::Custom;
        settings.updates = Updates {
            check: false,
            interval_hours: 168,
            last_check: Some(1_790_000_000),
            auto: false,
        };
        settings.remember_server(ServerEntry {
            url: "https://coroom.example.com".into(),
            fingerprint: Some("ab".repeat(32)),
            tcp_port: Some(8443),
            username: Some("ana".into()),
        });
        settings.remember_peer(KnownPeer {
            name: "Bea".into(),
            fingerprint: "cd".repeat(32),
            safety_number: "12345 67890".into(),
        });
        settings.audio.set_voice_volume("Carla", 60);
        settings.save(&path).expect("guardar");
        let text = std::fs::read_to_string(&path).expect("leer");
        assert!(text.contains("theme = \"dark\""), "{text}");
        assert!(text.contains("motion = \"reduced\""), "{text}");
        assert!(
            text.contains("file = \"/home/ana/sonidos/tono.ogg\""),
            "{text}"
        );
        assert!(
            text.contains("[notifications.member_left]\ntone = \"off\""),
            "{text}"
        );
        assert!(text.contains("tone = \"ping\""), "{text}");
        assert!(
            !text.contains("chat_sound"),
            "lo de antes no se escribe: {text}"
        );
        assert!(!temporary_path(&path).exists(), "sin temporales");
        let (loaded, warning) = Settings::load(&path);
        assert_eq!(warning, None);
        assert_eq!(loaded, settings);
    }

    /// Los ajustes de antes de las animaciones cargan con «Como el sistema».
    #[test]
    fn the_motion_choice_defaults_to_the_system() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = path_in(dir.path());
        std::fs::write(&path, "[appearance]\ntheme = \"dark\"\naccent = \"teal\"\n")
            .expect("escribir");
        let (loaded, warning) = Settings::load(&path);
        assert_eq!(warning, None);
        assert_eq!(loaded.appearance.theme, ThemeChoice::Dark);
        assert_eq!(loaded.appearance.motion, MotionChoice::System);
        for choice in [
            MotionChoice::Full,
            MotionChoice::Reduced,
            MotionChoice::System,
        ] {
            let mut settings = loaded.clone();
            settings.appearance.motion = choice;
            settings.save(&path).expect("guardar");
            assert_eq!(Settings::load(&path).0.appearance.motion, choice);
        }
    }

    #[test]
    fn the_motion_choice_combines_with_the_system() {
        let kde_slow = Motion::new(false, 2.0);
        let reduced = Motion::REDUCED;
        assert_eq!(MotionChoice::System.apply(Motion::FULL), Motion::FULL);
        assert_eq!(MotionChoice::System.apply(reduced), reduced);
        assert_eq!(MotionChoice::System.apply(Motion::OFF), Motion::OFF);
        // Reducidas: aunque el sistema no lo pida, y con su factor.
        assert_eq!(
            MotionChoice::Reduced.apply(kde_slow),
            Motion::new(true, 2.0)
        );
        assert!(MotionChoice::Reduced.apply(Motion::FULL).is_reduced());
        // Completas: aunque el sistema pida reducidas; con factor 0, las de siempre.
        assert_eq!(MotionChoice::Full.apply(reduced), Motion::FULL);
        assert_eq!(MotionChoice::Full.apply(kde_slow), kde_slow);
        assert_eq!(MotionChoice::Full.apply(Motion::OFF), Motion::FULL);
    }

    /// El temporal del guardado va junto a los ajustes, lleva el pid y no sobrevive al guardado.
    #[test]
    fn the_save_temporary_is_unique_per_process_and_cleaned_up() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = path_in(dir.path());
        let temporary = temporary_path(&path);
        assert_eq!(temporary.parent(), path.parent());
        assert_ne!(temporary, path);
        assert_eq!(
            temporary.extension().and_then(|ext| ext.to_str()),
            Some("tmp")
        );
        let name = temporary
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.contains(&std::process::id().to_string()),
            "{name} lleva el pid"
        );
        // Un temporal que otro proceso dejó a medias no estorba.
        std::fs::write(path.with_extension("toml.1.tmp"), "roto").expect("escribir");
        Settings::default().save(&path).expect("guardar");
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("listar")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert_eq!(left, ["settings.toml.1.tmp"]);
        assert!(!temporary.exists());
    }

    #[test]
    fn partial_and_broken_files_fall_back_to_defaults() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = path_in(dir.path());
        std::fs::write(&path, "[video]\nfps = 30\nfuturo = 1\n").expect("escribir");
        let (loaded, warning) = Settings::load(&path);
        assert_eq!(warning, None, "lo desconocido se ignora");
        assert_eq!(loaded.video.fps, 30);
        assert_eq!(loaded.audio, Audio::default());

        std::fs::write(&path, "[video]\nfps = 47\n").expect("escribir");
        assert_eq!(Settings::load(&path).0.video.fps, 60, "fps fuera de rango");
        std::fs::write(&path, "[updates]\ninterval_hours = 5\n").expect("escribir");
        let loaded = Settings::load(&path).0;
        assert_eq!(
            loaded.updates.interval_hours, 24,
            "intervalo fuera de las opciones"
        );
        assert!(loaded.updates.check, "se comprueba por defecto");
        assert!(
            loaded.updates.auto,
            "sin el campo (ajustes de antes), se actualiza sola"
        );
        std::fs::write(&path, "[audio]\nvoice = false\n").expect("escribir");
        let loaded = Settings::load(&path).0;
        assert!(
            loaded.audio.host_voice,
            "sin el campo (ajustes de antes), el anfitrión comparte el micrófono"
        );
        assert!(!loaded.audio.voice);

        std::fs::write(&path, "esto no es TOML = = =").expect("escribir");
        let (loaded, warning) = Settings::load(&path);
        assert_eq!(loaded, Settings::default());
        assert!(warning.is_some_and(|w| w.contains("por defecto")));
    }

    #[test]
    fn voice_volumes_remember_only_what_differs_from_normal() {
        let mut audio = Audio::default();
        assert!(audio.voice_volumes.is_empty(), "por defecto, nadie");
        assert_eq!(audio.voice_volume("Bea"), DEFAULT_VOICE_VOLUME);
        audio.set_voice_volume("Bea", 150);
        audio.set_voice_volume("Carla", 250);
        assert_eq!(audio.voice_volume("Bea"), 150);
        assert_eq!(audio.voice_volume("Carla"), MAX_VOICE_VOLUME, "acotado");
        audio.set_voice_volume("Bea", DEFAULT_VOICE_VOLUME);
        assert_eq!(audio.voice_volumes.len(), 1, "el normal no se guarda");
        assert_eq!(voice_gain(50), 0.5);
        assert_eq!(voice_gain(200), 2.0);
        assert_eq!(voice_gain(255), 2.0);
        // Ida y vuelta por TOML, y sin la tabla (ajustes de antes) queda vacía.
        let settings = Settings {
            audio,
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&settings).expect("escribir");
        assert!(text.contains("[audio.voice_volumes]"), "{text}");
        assert!(text.contains("Carla = 200"), "{text}");
        let loaded: Settings = toml::from_str(&text).expect("leer");
        assert_eq!(loaded, settings);
        let old: Settings = toml::from_str("[audio]\nvoice = true\n").expect("leer");
        assert!(old.audio.voice_volumes.is_empty());
        // Un fichero editado a mano se sanea: 100 se quita y lo que pasa de 200 se acota.
        let edited: Settings =
            toml::from_str("[audio.voice_volumes]\nAna = 100\nBea = 230\n").expect("leer");
        let sane = edited.sanitized();
        assert_eq!(sane.audio.voice_volumes.get("Ana"), None);
        assert_eq!(sane.audio.voice_volumes.get("Bea"), Some(&200));
    }

    #[test]
    fn local_mutes_are_remembered_by_name() {
        let mut audio = Audio::default();
        assert!(!audio.voice_muted("Bea"), "por defecto, nadie");
        audio.set_voice_muted("Bea", true);
        audio.set_voice_muted("Carla", true);
        audio.set_voice_muted("Carla", false);
        assert!(audio.voice_muted("Bea") && !audio.voice_muted("Carla"));
        assert_eq!(audio.voice_muted.len(), 1);
        // Ida y vuelta por TOML, junto a los volúmenes; sin la clave (ajustes de antes), nadie.
        audio.set_voice_volume("Bea", 150);
        let settings = Settings {
            audio,
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&settings).expect("escribir");
        let loaded: Settings = toml::from_str(&text).expect("leer");
        assert_eq!(loaded, settings, "{text}");
        let old: Settings = toml::from_str("[audio]\nvoice = true\n").expect("leer");
        assert!(old.audio.voice_muted.is_empty());
    }

    #[test]
    fn the_game_volume_is_normal_by_default_and_bounded() {
        let audio = Audio::default();
        assert_eq!(audio.game_volume, DEFAULT_VOICE_VOLUME);
        // Ida y vuelta por TOML, y sin la clave (ajustes de antes) queda al normal.
        let settings = Settings {
            audio: Audio {
                game_volume: 60,
                ..Audio::default()
            },
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&settings).expect("escribir");
        assert!(text.contains("game_volume = 60"), "{text}");
        let loaded: Settings = toml::from_str(&text).expect("leer");
        assert_eq!(loaded, settings);
        let old: Settings = toml::from_str("[audio]\nvoice = true\n").expect("leer");
        assert_eq!(old.audio.game_volume, DEFAULT_VOICE_VOLUME);
        // Un fichero editado a mano se sanea: lo que pasa de 200 se acota.
        let edited: Settings = toml::from_str("[audio]\ngame_volume = 230\n").expect("leer");
        assert_eq!(edited.sanitized().audio.game_volume, MAX_VOICE_VOLUME);
        let zero: Settings = toml::from_str("[audio]\ngame_volume = 0\n").expect("leer");
        assert_eq!(zero.sanitized().audio.game_volume, 0, "a cero se respeta");
    }

    #[test]
    fn quality_presets_apply_and_are_recognised() {
        let mut video = Video::default();
        assert_eq!(QualityPreset::matching(&video), QualityPreset::Balanced);
        QualityPreset::MinLatency.apply(&mut video);
        assert_eq!(video.max_resolution, Resolution::P1080);
        assert_eq!(video.preset, QualityPreset::MinLatency);
        QualityPreset::MaxQuality.apply(&mut video);
        assert_eq!(video.fps, 120);
        assert!(video.chroma_444);
        video.fps = 30;
        assert_eq!(QualityPreset::matching(&video), QualityPreset::Custom);
        // Un perfil guardado que no cuadra con las opciones se corrige al sanear.
        let mut settings = Settings::default();
        settings.video.fps = 30;
        settings.video.preset = QualityPreset::Balanced;
        assert_eq!(settings.sanitized().video.preset, QualityPreset::Custom);
        assert_eq!(Video::when(VideoOption::Resolution), When::Live);
        assert_eq!(Video::when(VideoOption::Fps), When::Live);
        assert_eq!(Video::when(VideoOption::Chroma444), When::NextRoom);
        assert_eq!(Video::when(VideoOption::Hdr), When::NextRoom);
    }

    #[test]
    fn shortcuts_must_be_distinct_letters() {
        assert!(Shortcuts::default().is_valid());
        let repeated = Shortcuts {
            stats: 'g',
            ..Shortcuts::default()
        };
        assert!(!repeated.is_valid());
        let symbol = Shortcuts {
            mic: '1',
            ..Shortcuts::default()
        };
        assert!(!symbol.is_valid());
        let mut settings = Settings::default();
        settings.input.shortcuts = repeated;
        assert_eq!(settings.sanitized().input.shortcuts, Shortcuts::default());
    }

    /// Unos ajustes de antes del atajo de soltar con su letra (`R`) ya en otro atajo: ese se
    /// respeta y soltar toma una libre.
    #[test]
    fn the_release_shortcut_takes_a_free_letter_in_old_settings() {
        let old: Settings =
            toml::from_str("[input.shortcuts]\nchat = \"R\"\nstats = \"K\"\n").expect("leer");
        let shortcuts = old.sanitized().input.shortcuts;
        assert_eq!(shortcuts.chat, 'R');
        assert_eq!(shortcuts.stats, 'K');
        assert_ne!(shortcuts.release, 'R');
        assert!(shortcuts.is_valid(), "{shortcuts:?}");
        assert_eq!(
            Settings::default().sanitized().input.shortcuts.release,
            'R',
            "sin choque, la de fábrica"
        );
    }

    /// Lo mismo con apagar el vídeo (`I`): unos ajustes de antes con la I en otro atajo la
    /// conservan y el vídeo toma una libre; sin choque, la I.
    #[test]
    fn the_video_shortcut_takes_a_free_letter_in_old_settings() {
        let old: Settings = toml::from_str("[input.shortcuts]\nchat = \"I\"\n").expect("leer");
        let shortcuts = old.sanitized().input.shortcuts;
        assert_eq!(shortcuts.chat, 'I');
        assert_ne!(shortcuts.video, 'I');
        assert!(shortcuts.is_valid(), "{shortcuts:?}");
        assert_eq!(Settings::default().sanitized().input.shortcuts.video, 'I');
        let old: Settings = toml::from_str("[input.shortcuts]\nmic = \"K\"\n").expect("leer");
        assert_eq!(old.sanitized().input.shortcuts.video, 'I');
    }

    #[test]
    fn servers_keep_the_last_one_first() {
        let mut settings = Settings::default();
        let entry = |url: &str| ServerEntry {
            url: url.into(),
            fingerprint: None,
            tcp_port: None,
            username: None,
        };
        settings.remember_server(entry("https://a.example.com"));
        settings.remember_server(entry("https://b.example.com"));
        settings.remember_server(entry("https://a.example.com"));
        let urls: Vec<&str> = settings.servers.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(urls, ["https://a.example.com", "https://b.example.com"]);
        settings.forget_server("https://a.example.com");
        assert_eq!(settings.servers.len(), 1);
    }

    /// El sonido del chat de antes pasa a «Notificaciones»: apagado, con fichero propio o, sin
    /// tocar, la campanilla; y lo de antes ya no se vuelve a escribir.
    #[test]
    fn the_old_chat_sound_moves_to_notifications() {
        let load = |text: &str| -> Settings {
            toml::from_str::<Settings>(text).expect("leer").sanitized()
        };
        let off = load("[room]\nchat_sound = false\nchat_bubbles = false\n");
        assert_eq!(off.notifications.chat_message.tone, SoundChoice::Off);
        assert!(!off.room.chat_bubbles, "el resto de la sala se respeta");
        assert_eq!(
            off.notifications.member_joined,
            Notifications::default().member_joined,
            "los avisos nuevos, con su tono"
        );

        let custom = load("[room]\nchat_sound = true\nchat_sound_file = \"/home/ana/tono.ogg\"\n");
        assert_eq!(custom.notifications.chat_message.tone, SoundChoice::File);
        assert_eq!(
            custom.notifications.chat_message.file.as_deref(),
            Some(Path::new("/home/ana/tono.ogg"))
        );
        // Apagado con un fichero elegido: sigue apagado, y el fichero se recuerda.
        let both = load("[room]\nchat_sound = false\nchat_sound_file = \"/home/ana/tono.ogg\"\n");
        assert_eq!(both.notifications.chat_message.tone, SoundChoice::Off);
        assert!(both.notifications.chat_message.file.is_some());

        let untouched = load("[room]\nchat_sound = true\n");
        assert_eq!(
            untouched.notifications.chat_message,
            EventSound::tone(Builtin::Chime)
        );
        assert_eq!(untouched.notifications.volume, DEFAULT_NOTIFICATION_VOLUME);

        let text = toml::to_string_pretty(&custom).expect("escribir");
        assert!(!text.contains("chat_sound"), "{text}");
        let again: Settings = toml::from_str(&text).expect("leer");
        assert_eq!(again.sanitized(), custom, "la migración se queda");
    }

    /// Un aviso que no se entiende (un tono que no existe, una tabla sin `tone`, un volumen que
    /// no es un número) vuelve a lo de por defecto de ese aviso; el resto de los ajustes, también
    /// de los otros avisos, se queda como estaba y no hay aviso de ajustes dañados.
    #[test]
    fn a_bad_notification_value_keeps_the_other_settings() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = path_in(dir.path());
        std::fs::write(
            &path,
            "[video]\nfps = 30\n\
             [notifications]\nvolume = \"alto\"\n\
             [notifications.invitation]\ntone = \"gong\"\n\
             [notifications.chat_message]\nfile = \"/home/ana/tono.ogg\"\n\
             [notifications.member_left]\ntone = 7\n\
             [notifications.join_request]\ntone = \"ping\"\n\
             [notifications.member_joined]\ntone = \"off\"\n",
        )
        .expect("escribir");
        let (loaded, warning) = Settings::load(&path);
        assert_eq!(warning, None);
        assert_eq!(loaded.video.fps, 30, "lo de fuera de los avisos sigue");
        let notifications = &loaded.notifications;
        assert_eq!(notifications.volume, DEFAULT_NOTIFICATION_VOLUME);
        assert_eq!(
            notifications.invitation,
            EventSound::tone(SoundEvent::Invitation.default_tone()),
            "un tono que no existe"
        );
        assert_eq!(
            notifications.chat_message.tone,
            SoundChoice::Tone(SoundEvent::ChatMessage.default_tone()),
            "sin «tone»"
        );
        assert_eq!(
            notifications.chat_message.file.as_deref(),
            Some(Path::new("/home/ana/tono.ogg")),
            "el fichero se recuerda"
        );
        assert_eq!(
            notifications.member_left,
            EventSound::tone(SoundEvent::MemberLeft.default_tone())
        );
        assert_eq!(
            notifications.join_request.tone,
            SoundChoice::Tone(Builtin::Ping),
            "los otros avisos siguen como estaban"
        );
        assert_eq!(notifications.member_joined.tone, SoundChoice::Off);
        // Guardar y volver a leer da lo mismo.
        let text = toml::to_string_pretty(&loaded).expect("escribir");
        assert_eq!(toml::from_str::<Settings>(&text).expect("leer"), loaded);
    }

    #[test]
    fn notification_settings_are_sanitized() {
        let edited: Settings = toml::from_str(
            "[notifications]\nvolume = 180\n[notifications.invitation]\ntone = \"file\"\n",
        )
        .expect("leer");
        let sane = edited.sanitized();
        assert_eq!(sane.notifications.volume, 100, "el volumen no pasa de 100");
        assert_eq!(
            sane.notifications.invitation.tone,
            SoundChoice::Tone(SoundEvent::Invitation.default_tone()),
            "«fichero» sin fichero vuelve a su tono"
        );
        // Cada aviso tiene su tono, y todos distintos.
        let defaults: BTreeSet<Builtin> = SoundEvent::ALL
            .iter()
            .map(|event| event.default_tone())
            .collect();
        assert_eq!(defaults.len(), SoundEvent::ALL.len());
        assert!(
            SoundEvent::ALL
                .iter()
                .all(|event| Notifications::default().sound(*event).is_on())
        );
        assert!((Notifications::default().gain() - 0.64).abs() < 1e-6);
        let quiet = Notifications {
            volume: 0,
            ..Notifications::default()
        };
        assert_eq!(quiet.gain(), 0.0);
    }
}
