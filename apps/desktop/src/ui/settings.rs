//! Ajustes de la aplicación, guardados en `COROOM_DATA_DIR/settings.toml` (por defecto, el
//! directorio de datos del usuario: [`crate::remote::default_data_dir`]).
//!
//! El fichero es TOML legible y tolerante: lo que falta toma su valor por defecto y lo que no se
//! entiende se ignora (con un aviso), así una versión más nueva o una edición a mano no dejan la
//! aplicación sin arrancar. Se escribe entero en un fichero temporal y se renombra (nunca queda a
//! medias). No guarda secretos: las credenciales van al llavero ([`crate::account`]).

use std::path::{Path, PathBuf};

use coroom_session::input::gamepad::PadChoice;
use serde::{Deserialize, Serialize};

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

/// Apariencia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Claro, oscuro o el del sistema.
    pub theme: ThemeChoice,
    /// Acento: `"system"` o una clave de [`coroom_ui::theme::Accent::key`].
    pub accent: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            accent: "system".to_owned(),
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
                video.max_bitrate_kbps = 10_000;
                video.chroma_444 = false;
            }
            QualityPreset::Balanced => {
                video.max_resolution = Resolution::P2160;
                video.fps = 60;
                video.max_bitrate_kbps = 20_000;
                video.chroma_444 = false;
            }
            QualityPreset::MaxQuality => {
                video.max_resolution = Resolution::P2160;
                video.fps = 120;
                video.max_bitrate_kbps = coroom_session::remote::UNLIMITED_KBPS;
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
                    && probe.max_bitrate_kbps == video.max_bitrate_kbps
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
    /// Tope del bitrate Auto.
    Bitrate,
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
    /// Bitrate máximo en kbit/s (el control de tasa lo baja si hace falta).
    pub max_bitrate_kbps: u32,
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
            max_bitrate_kbps: 20_000,
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
    /// la sala abierta (se reabre el codificador con un intra) y el tope del bitrate también (sin
    /// renegociar nada); 4:4:4 y HDR, al abrir la próxima (cambian cómo se pide la captura, en
    /// GPU o en CPU, y eso solo se decide al arrancarla).
    pub fn when(option: VideoOption) -> When {
        match option {
            VideoOption::Codec
            | VideoOption::Bitrate
            | VideoOption::Resolution
            | VideoOption::Fps => When::Live,
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
    /// Sonido al llegar un mensaje del chat.
    pub chat_sound: bool,
    /// Fichero del tono (`None`: el de por defecto; ver `ui::sound`). En Windows solo WAV.
    pub chat_sound_file: Option<PathBuf>,
    /// Notificación del escritorio con la ventana sin foco.
    pub chat_notifications: bool,
    /// Burbuja de 5 s sobre el vídeo con el panel cerrado.
    pub chat_bubbles: bool,
}

impl Default for Room {
    fn default() -> Self {
        Self {
            guests_muted: false,
            chat_sound: true,
            chat_sound_file: None,
            chat_notifications: true,
            chat_bubbles: true,
        }
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
        }
    }
}

/// Atajos del invitado (siempre con `Ctrl+Alt`): la letra de cada uno.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    /// Inmersivo / escritorio.
    pub immersive: char,
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
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            immersive: 'G',
            stats: 'S',
            chat: 'C',
            leave: 'Q',
            mic: 'M',
            game: 'V',
            pads: 'J',
        }
    }
}

impl Shortcuts {
    /// Las siete letras, en orden.
    pub fn letters(&self) -> [char; 7] {
        [
            self.immersive,
            self.stats,
            self.chat,
            self.leave,
            self.mic,
            self.game,
            self.pads,
        ]
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
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check: true,
            interval_hours: 24,
            last_check: None,
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
    /// Aviso de versión nueva.
    pub updates: Updates,
    /// Servidores conocidos (el primero, el último usado).
    pub servers: Vec<ServerEntry>,
    /// Pares vistos en las salas.
    pub peers: Vec<KnownPeer>,
}

/// Valores admitidos de fps.
pub const FPS_CHOICES: [u32; 3] = [30, 60, 120];

/// Topes del bitrate que se ofrecen (kbit/s). El último es «sin tope»
/// ([`coroom_session::remote::UNLIMITED_KBPS`]): Auto sube hasta donde la red aguante.
pub const BITRATE_CHOICES: [u32; 8] = [
    5_000,
    10_000,
    20_000,
    30_000,
    50_000,
    80_000,
    150_000,
    coroom_session::remote::UNLIMITED_KBPS,
];

impl Settings {
    /// Corrige valores fuera de rango (por ejemplo, de una edición a mano).
    pub fn sanitized(mut self) -> Self {
        if !FPS_CHOICES.contains(&self.video.fps) {
            self.video.fps = Video::default().fps;
        }
        self.video.max_bitrate_kbps = self
            .video
            .max_bitrate_kbps
            .clamp(500, coroom_session::remote::UNLIMITED_KBPS);
        if self.video.preset != QualityPreset::Custom
            && QualityPreset::matching(&self.video) != self.video.preset
        {
            // El perfil guardado y las opciones no cuadran (ajustes de otra versión): manda lo
            // que hay en las opciones.
            self.video.preset = QualityPreset::matching(&self.video);
        }
        if !self.input.shortcuts.is_valid() {
            self.input.shortcuts = Shortcuts::default();
        }
        self.input.preferred_slot = self.input.preferred_slot.min(8);
        if coroom_session::input::keymap::keycode_from_name(&self.audio.push_to_talk_key).is_none()
        {
            self.audio.push_to_talk_key = Audio::default().push_to_talk_key;
        }
        let mut seen = std::collections::BTreeSet::new();
        self.servers
            .retain(|server| seen.insert(server.url.clone()));
        self.peers.truncate(64);
        if !crate::update::INTERVAL_CHOICES.contains(&self.updates.interval_hours) {
            self.updates.interval_hours = Updates::default().interval_hours;
        }
        self
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
        settings.video.fps = 120;
        settings.video.codec = CodecChoice::Av1;
        settings.video.max_resolution = Resolution::P1440;
        settings.audio.voice_mode = VoiceMode::PushToTalk;
        settings.audio.push_to_talk_key = "CapsLock".into();
        settings.network.force_relay = true;
        settings.network.local_port = 47_000;
        settings.room.chat_sound_file = Some(PathBuf::from("/home/ana/sonidos/tono.ogg"));
        settings.input.shortcuts.immersive = 'K';
        settings.video.chroma_444 = true;
        settings.video.hdr = true;
        // Tocar Avanzado deja el perfil en «Personalizado» (al cargar se comprueba que cuadre).
        settings.video.preset = QualityPreset::Custom;
        settings.updates = Updates {
            check: false,
            interval_hours: 168,
            last_check: Some(1_790_000_000),
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
        settings.save(&path).expect("guardar");
        let text = std::fs::read_to_string(&path).expect("leer");
        assert!(text.contains("theme = \"dark\""), "{text}");
        assert!(
            text.contains("chat_sound_file = \"/home/ana/sonidos/tono.ogg\""),
            "{text}"
        );
        assert!(!temporary_path(&path).exists(), "sin temporales");
        let (loaded, warning) = Settings::load(&path);
        assert_eq!(warning, None);
        assert_eq!(loaded, settings);
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
    fn quality_presets_apply_and_are_recognised() {
        let mut video = Video::default();
        assert_eq!(QualityPreset::matching(&video), QualityPreset::Balanced);
        QualityPreset::MinLatency.apply(&mut video);
        assert_eq!(video.max_resolution, Resolution::P1080);
        assert_eq!(video.max_bitrate_kbps, 10_000);
        assert_eq!(video.preset, QualityPreset::MinLatency);
        QualityPreset::MaxQuality.apply(&mut video);
        assert_eq!(video.fps, 120);
        assert!(video.chroma_444);
        assert_eq!(
            video.max_bitrate_kbps,
            coroom_session::remote::UNLIMITED_KBPS
        );
        video.fps = 30;
        assert_eq!(QualityPreset::matching(&video), QualityPreset::Custom);
        // Un perfil guardado que no cuadra con las opciones se corrige al sanear.
        let mut settings = Settings::default();
        settings.video.fps = 30;
        settings.video.preset = QualityPreset::Balanced;
        assert_eq!(settings.sanitized().video.preset, QualityPreset::Custom);
        assert_eq!(Video::when(VideoOption::Bitrate), When::Live);
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
}
