//! Binario de escritorio `coroom` (anfitrión e invitado).
//!
//! Tiene la línea de órdenes en español, `coroom doctor`, la prueba de latencia
//! local `coroom probe` (con ventana o sin ella), la sesión remota: `coroom host`
//! (crea la sala y comparte la pantalla) y `coroom join` (entra y la muestra), y
//! las cuentas: `coroom login`, `logout`, `whoami`, `friends` e `invite`. Sin
//! argumentos abre la aplicación (`coroom_desktop::ui`), y `probe` y `join` con ventana usan su
//! pantalla de sala.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use coroom_desktop::account::{self, AccountOptions, FriendsAction, LoginOptions};
use coroom_desktop::probe::{ProbeOptions, Source, run_headless};
use coroom_desktop::remote::{
    AutoAllow, CONSOLE_HELP, HostAudioOptions, HostOptions, JoinAudioOptions, JoinOptions,
    NetOptions, load_identity, parse_game_capture, print_events_forwarding, run_host_console,
    run_join_headless, start_host, stdin_lines, wait_for_invitation,
};
use coroom_desktop::ui::{self, Launch};
use coroom_session::codec::{Backend, Codec, DecoderBackend};
use coroom_session::latency::LatencySummary;
use coroom_session::loopback::{LoopbackConfig, LoopbackPipeline};
use coroom_session::remote::RemoteSession;
use coroom_session::{Session, SessionError};

/// Juego remoto con amigos, con cifrado de extremo a extremo.
#[derive(Debug, Parser)]
#[command(
    name = "coroom",
    version,
    about = "Juego remoto con amigos, con cifrado de extremo a extremo.",
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    next_help_heading = "Opciones",
    subcommand_help_heading = "Comandos",
    subcommand_value_name = "COMANDO",
    override_usage = "coroom [OPCIONES] [COMANDO]",
    help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Muestra esta ayuda.
    #[arg(short = 'h', long = "help", action = ArgAction::Help, global = true, display_order = 1000)]
    help: Option<bool>,

    /// Muestra la versión.
    #[arg(short = 'V', long = "version", action = ArgAction::Version, display_order = 1001)]
    version: Option<bool>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Comprueba el sistema: protocolo, identidad, captura, adaptador gráfico y códecs (FFmpeg).
    #[command(
        disable_help_flag = true,
        next_help_heading = "Opciones",
        override_usage = "coroom doctor [OPCIONES]",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Doctor,

    /// Prueba de latencia local: captura → códec → UDP local → decodificación → presentación.
    #[command(
        disable_help_flag = true,
        next_help_heading = "Opciones",
        override_usage = "coroom probe [OPCIONES]",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Probe(ProbeArgs),

    /// Crea una sala y comparte la pantalla (o la fuente sintética) con los invitados.
    #[command(
        disable_help_flag = true,
        next_help_heading = "Opciones",
        override_usage = "coroom host --server URL [OPCIONES]",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Host(HostArgs),

    /// Entra en una sala con su código (o espera la invitación de un amigo) y muestra la
    /// pantalla del anfitrión.
    #[command(
        disable_help_flag = true,
        next_help_heading = "Opciones",
        override_usage = "coroom join [--server URL] [OPCIONES] [CÓDIGO]",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Join(JoinArgs),

    /// Inicia sesión en un servidor (o crea la cuenta con un código de invitación) y vincula
    /// este equipo.
    #[command(
        disable_help_flag = true,
        next_help_heading = "Opciones",
        override_usage = "coroom login --server URL [--invitation CÓDIGO] [OPCIONES]",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Login(LoginArgs),

    /// Cierra la sesión en el servidor y borra sus credenciales de este equipo.
    #[command(
        disable_help_flag = true,
        next_help_heading = "Opciones",
        override_usage = "coroom logout [--server URL]",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Logout(AccountArgs),

    /// Muestra con qué cuenta y dispositivo has iniciado sesión y tu uso del relay.
    #[command(
        disable_help_flag = true,
        next_help_heading = "Opciones",
        override_usage = "coroom whoami [--server URL]",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Whoami(AccountArgs),

    /// Amigos: lista (con quién está conectado), pedir, aceptar y quitar.
    #[command(
        disable_help_flag = true,
        subcommand_help_heading = "Comandos",
        subcommand_value_name = "ACCIÓN",
        override_usage = "coroom friends list|add NOMBRE|accept NOMBRE|remove NOMBRE",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Friends {
        /// Acción.
        #[command(subcommand)]
        action: FriendsCommand,
    },

    /// Invita a un amigo conectado a tu sala (la que tienes abierta con `coroom host`).
    #[command(
        disable_help_flag = true,
        next_help_heading = "Opciones",
        override_usage = "coroom invite NOMBRE --code CÓDIGO [--server URL]",
        help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
    )]
    Invite(InviteArgs),
}

/// Opciones comunes de las órdenes de cuenta.
#[derive(Debug, Clone, Args)]
struct AccountArgs {
    /// URL del servidor (por defecto, el último en el que iniciaste sesión).
    #[arg(long, value_name = "URL")]
    server: Option<String>,

    /// Huella SHA-256 del certificado autofirmado del servidor (se guarda al iniciar sesión).
    #[arg(long, value_name = "SHA256")]
    trust_fingerprint: Option<String>,

    /// Puerto TCP del respaldo HTTP/2 si no es el de la URL (servidores de desarrollo).
    #[arg(long, value_name = "PUERTO")]
    tcp_port: Option<u16>,

    /// Directorio de datos (identidad y credenciales; por defecto, el llavero o el del usuario).
    #[arg(long, value_name = "DIRECTORIO")]
    data_dir: Option<PathBuf>,

    /// Sin llavero: identidad y credenciales en el directorio de datos.
    #[arg(long)]
    headless: bool,
}

impl AccountArgs {
    fn options(&self) -> AccountOptions {
        AccountOptions {
            server: self.server.clone(),
            trust_fingerprint: self.trust_fingerprint.clone(),
            tcp_port: self.tcp_port,
            data_dir: self.data_dir.clone(),
            headless: self.headless,
        }
    }
}

#[derive(Debug, Args)]
struct LoginArgs {
    #[command(flatten)]
    account: AccountArgs,

    /// Código de invitación: crea la cuenta (pide nombre y contraseña) y después inicia sesión.
    #[arg(long, value_name = "CÓDIGO")]
    invitation: Option<String>,

    /// Usuario (si no, se pregunta).
    #[arg(long, value_name = "NOMBRE")]
    user: Option<String>,

    /// Nombre visible de la cuenta nueva (con --invitation).
    #[arg(long, value_name = "NOMBRE")]
    display_name: Option<String>,

    /// Lee la contraseña de la entrada estándar (una línea) en vez de pedirla sin eco.
    #[arg(long)]
    password_stdin: bool,

    /// Nombre de este dispositivo en la cuenta (por defecto, el del equipo).
    #[arg(long, value_name = "NOMBRE")]
    device_name: Option<String>,
}

#[derive(Debug, Args)]
struct FriendArgs {
    /// Nombre de usuario del amigo.
    #[arg(value_name = "NOMBRE")]
    name: String,

    #[command(flatten)]
    account: AccountArgs,
}

#[derive(Debug, Subcommand)]
enum FriendsCommand {
    /// Lista de amigos (● conectado) y solicitudes pendientes.
    #[command(disable_help_flag = true)]
    List(AccountArgs),
    /// Pide amistad (si ya te la pidió, quedáis como amigos).
    #[command(disable_help_flag = true)]
    Add(FriendArgs),
    /// Acepta la solicitud de amistad de NOMBRE.
    #[command(disable_help_flag = true)]
    Accept(FriendArgs),
    /// Quita a NOMBRE de tus amigos (o retira o rechaza su solicitud).
    #[command(disable_help_flag = true)]
    Remove(FriendArgs),
}

#[derive(Debug, Args)]
struct InviteArgs {
    /// Nombre de usuario del amigo.
    #[arg(value_name = "NOMBRE")]
    name: String,

    /// Código de tu sala.
    #[arg(long, value_name = "CÓDIGO")]
    code: String,

    #[command(flatten)]
    account: AccountArgs,
}

/// Opciones de red de `host` y `join`.
#[derive(Debug, Args)]
struct NetArgs {
    /// URL del servidor de coroom, por ejemplo https://coroom.example.com (por defecto, el
    /// último en el que iniciaste sesión).
    #[arg(long, value_name = "URL")]
    server: Option<String>,

    /// Usa siempre el relay del servidor, sin intentar la conexión directa.
    #[arg(long)]
    force_relay: bool,

    /// Huella SHA-256 del certificado autofirmado del servidor (la que imprime al arrancar).
    #[arg(long, value_name = "SHA256")]
    trust_fingerprint: Option<String>,

    /// Solo servidores en modo desarrollo: token de desarrollo en lugar de la cuenta
    /// [COROOM_DEV_TOKEN].
    #[arg(long, env = "COROOM_DEV_TOKEN", hide_env = true, value_name = "TOKEN")]
    dev_token: Option<String>,

    /// Directorio de datos con la identidad (por defecto, el llavero o el del usuario).
    #[arg(long, value_name = "DIRECTORIO")]
    data_dir: Option<PathBuf>,

    /// Solo desarrollo: medios sin SFrame, para la prueba A/B (los dos extremos igual).
    #[cfg(debug_assertions)]
    #[arg(long)]
    insecure_no_sframe: bool,
}

impl NetArgs {
    fn options(&self, headless: bool) -> NetOptions {
        NetOptions {
            server: self.server.clone(),
            dev_token: self.dev_token.clone().filter(|token| !token.is_empty()),
            trust_fingerprint: self.trust_fingerprint.clone(),
            force_relay: self.force_relay,
            data_dir: self.data_dir.clone(),
            headless,
            local_port: None,
            #[cfg(debug_assertions)]
            insecure_no_sframe: self.insecure_no_sframe,
        }
    }
}

#[derive(Debug, Args)]
struct HostArgs {
    #[command(flatten)]
    net: NetArgs,

    /// Fuente de vídeo: `screen` (por defecto) o `synthetic`.
    #[arg(
        long,
        value_enum,
        default_value = "screen",
        hide_default_value = true,
        hide_possible_values = true,
        value_name = "FUENTE"
    )]
    source: SourceArg,

    /// Ancho en píxeles (por defecto 1920).
    #[arg(long, default_value_t = 1920, hide_default_value = true, value_name = "PÍXELES",
          value_parser = clap::value_parser!(u32).range(16..=3840))]
    width: u32,

    /// Alto en píxeles (por defecto 1080).
    #[arg(long, default_value_t = 1080, hide_default_value = true, value_name = "PÍXELES",
          value_parser = clap::value_parser!(u32).range(16..=2160))]
    height: u32,

    /// Fotogramas por segundo (por defecto 60).
    #[arg(long, default_value_t = 60, hide_default_value = true, value_name = "FPS",
          value_parser = clap::value_parser!(u32).range(1..=240))]
    fps: u32,

    /// Bitrate máximo en kbit/s (por defecto 10000); el control de tasa lo baja si hace falta.
    #[arg(long = "bitrate-kbps", default_value_t = 10_000, hide_default_value = true,
          value_name = "KBIT/S", value_parser = clap::value_parser!(u32).range(100..=500_000))]
    bitrate_kbps: u32,

    /// Termina tras enviar N fotogramas (por defecto, al pulsar Intro).
    #[arg(long, value_name = "N")]
    frames: Option<u64>,

    /// Códec: `auto` (por defecto, el mejor común con los invitados), `av1`, `hevc` o `h264`.
    #[arg(
        long,
        value_enum,
        default_value = "auto",
        hide_default_value = true,
        hide_possible_values = true,
        value_name = "CÓDEC"
    )]
    codec: HostCodecArg,

    /// Codificador: `auto` (por defecto), `software`, `vulkan`, `nvenc`, `amf`, `qsv` o `vaapi`.
    #[arg(
        long,
        value_enum,
        default_value = "auto",
        hide_default_value = true,
        hide_possible_values = true,
        value_name = "BACKEND"
    )]
    encoder: EncoderArg,

    /// Croma 4:4:4 si el codificador y todos los invitados lo admiten.
    #[arg(long)]
    yuv444: bool,

    /// HDR10 (10 bits, BT.2020 PQ) si la captura, el codificador y todos los invitados lo
    /// admiten; si no, SDR.
    #[arg(long)]
    hdr: bool,

    /// Permisos para cada invitado que entra: `kbm`, `pad` (primera ranura libre) o `kbm,pad`.
    #[arg(long, value_name = "PERMISOS", value_parser = AutoAllow::parse)]
    auto_allow: Option<AutoAllow>,

    /// Windows: los mandos de los invitados se convierten en teclas (perfil fijo) en vez de usar
    /// el driver de mando virtual (drivers/windows-vhid).
    #[arg(long)]
    gamepad_to_keys: bool,

    /// Audio del juego: `system` (por defecto, lo que suena), `none`, `apps-except A,B` o `apps-only A,B` (mezcla por aplicación, por clave), `app NOMBRE` (Linux, el nodo
    /// de PipeWire de esa aplicación) o `process PID` (Windows, ese proceso y sus hijos).
    #[arg(long, num_args = 1..=2, value_names = ["MODO", "NOMBRE"])]
    audio: Option<Vec<String>>,

    /// Habla por la sala con el micrófono (los invitados necesitan el permiso `voice`).
    #[arg(long)]
    voice: bool,

    /// Micrófono por su nombre (por defecto, el del sistema; mira `coroom doctor`).
    #[arg(long, value_name = "NOMBRE")]
    mic: Option<String>,

    /// Salida por su nombre para la voz de los invitados (por defecto, la del sistema).
    #[arg(long, value_name = "NOMBRE")]
    speaker: Option<String>,

    /// Paquetes de audio de 10 ms (por defecto) o de 20 ms, si la tasa de paquetes aprieta.
    #[arg(long = "audio-frame-ms", default_value_t = 10, hide_default_value = true,
          value_name = "MS", value_parser = clap::value_parser!(u8).range(10..=20))]
    audio_frame_ms: u8,

    /// Invita a este amigo (conectado) en cuanto exista la sala; se puede repetir.
    #[arg(long = "invite", value_name = "NOMBRE")]
    invites: Vec<String>,

    /// Sin llavero: la identidad va al directorio de datos.
    #[arg(long)]
    headless: bool,

    /// Imprime el resumen final en JSON (los avisos van a la salida de error).
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct JoinArgs {
    #[command(flatten)]
    net: NetArgs,

    /// Código de la sala (6 caracteres); sin él, espera invitaciones de tus amigos.
    #[arg(value_name = "CÓDIGO")]
    code: Option<String>,

    /// Sin código: entra con la primera invitación que llegue, sin preguntar.
    #[arg(long)]
    first: bool,

    /// Fotogramas que presentar; 0 = hasta cerrar (por defecto 0 con ventana y 600 sin ella).
    #[arg(long, value_name = "N")]
    frames: Option<u64>,

    /// Decodificador: `auto` (por defecto), `software`, `vulkan`, `nvdec`, `d3d12va`, `d3d11va` o
    /// `vaapi`.
    #[arg(
        long,
        value_enum,
        default_value = "auto",
        hide_default_value = true,
        hide_possible_values = true,
        value_name = "BACKEND"
    )]
    decoder: DecoderArg,

    /// Acepta 4:4:4 si el anfitrión lo envía y este equipo lo decodifica.
    #[arg(long)]
    yuv444: bool,

    /// Acepta HDR10 si el anfitrión lo envía y este equipo lo decodifica.
    #[arg(long)]
    hdr: bool,

    /// Sin ventana: presentación nula y resumen al terminar.
    #[arg(long)]
    headless: bool,

    /// Sin ventana: reproduce este guion de entrada JSON (y termina con él si no hay --frames).
    #[arg(long, value_name = "FICHERO", requires = "headless")]
    input_script: Option<PathBuf>,

    /// Empieza con el audio del juego silenciado (`Ctrl+Alt+V` lo alterna).
    #[arg(long)]
    mute_game: bool,

    /// Habla por la sala con el micrófono, si el anfitrión te da el permiso de voz
    /// (`Ctrl+Alt+M` lo silencia).
    #[arg(long)]
    voice: bool,

    /// Pulsar para hablar con esta tecla (`F12`, `CapsLock`, `v`…); sin ella, activación por voz.
    #[arg(long, value_name = "TECLA", requires = "voice", value_parser = parse_ptt)]
    ptt: Option<String>,

    /// Micrófono por su nombre (por defecto, el del sistema; mira `coroom doctor`).
    #[arg(long, value_name = "NOMBRE")]
    mic: Option<String>,

    /// Salida de audio por su nombre (por defecto, la del sistema).
    #[arg(long, value_name = "NOMBRE")]
    speaker: Option<String>,

    /// Sin audio: ni el del juego ni las voces.
    #[arg(long)]
    no_audio: bool,

    /// Imprime el resumen final en JSON (los avisos van a la salida de error).
    #[arg(long)]
    json: bool,
}

/// Códec de `coroom host`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum HostCodecArg {
    /// El mejor común con los invitados.
    Auto,
    /// AV1.
    Av1,
    /// HEVC.
    Hevc,
    /// H.264.
    H264,
}

impl HostCodecArg {
    fn codec(self) -> Option<Codec> {
        match self {
            HostCodecArg::Auto => None,
            HostCodecArg::Av1 => Some(Codec::Av1),
            HostCodecArg::Hevc => Some(Codec::Hevc),
            HostCodecArg::H264 => Some(Codec::H264),
        }
    }
}

/// Códec de `coroom probe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CodecArg {
    /// H.264.
    H264,
    /// HEVC.
    Hevc,
    /// AV1.
    Av1,
}

impl CodecArg {
    fn codec(self) -> Codec {
        match self {
            CodecArg::H264 => Codec::H264,
            CodecArg::Hevc => Codec::Hevc,
            CodecArg::Av1 => Codec::Av1,
        }
    }
}

/// Backend de codificación.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EncoderArg {
    /// El mejor que funcione.
    Auto,
    /// SVT-AV1 u OpenH264.
    Software,
    /// Vulkan Video.
    Vulkan,
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD AMF.
    Amf,
    /// Intel QSV.
    Qsv,
    /// VA-API.
    Vaapi,
}

impl EncoderArg {
    fn backend(self) -> Option<Backend> {
        match self {
            EncoderArg::Auto => None,
            EncoderArg::Software => Some(Backend::Software),
            EncoderArg::Vulkan => Some(Backend::VulkanVideo),
            EncoderArg::Nvenc => Some(Backend::Nvenc),
            EncoderArg::Amf => Some(Backend::Amf),
            EncoderArg::Qsv => Some(Backend::Qsv),
            EncoderArg::Vaapi => Some(Backend::Vaapi),
        }
    }
}

/// Backend de decodificación.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DecoderArg {
    /// El mejor que funcione.
    Auto,
    /// dav1d, OpenH264 o `hevc` de FFmpeg.
    Software,
    /// Vulkan Video.
    Vulkan,
    /// NVIDIA NVDEC.
    Nvdec,
    /// Direct3D 12 Video.
    D3d12va,
    /// Direct3D 11 Video.
    D3d11va,
    /// VA-API.
    Vaapi,
}

impl DecoderArg {
    fn backend(self) -> Option<DecoderBackend> {
        match self {
            DecoderArg::Auto => None,
            DecoderArg::Software => Some(DecoderBackend::Software),
            DecoderArg::Vulkan => Some(DecoderBackend::Vulkan),
            DecoderArg::Nvdec => Some(DecoderBackend::Nvdec),
            DecoderArg::D3d12va => Some(DecoderBackend::D3D12va),
            DecoderArg::D3d11va => Some(DecoderBackend::D3D11va),
            DecoderArg::Vaapi => Some(DecoderBackend::Vaapi),
        }
    }
}

/// Comprueba la tecla de `--ptt`.
fn parse_ptt(name: &str) -> Result<String, String> {
    coroom_session::input::keymap::keycode_from_name(name)
        .map(|_| name.trim().to_owned())
        .ok_or_else(|| format!("no conozco la tecla «{name}» (prueba F12, CapsLock o una letra)"))
}

impl HostArgs {
    fn options(&self) -> Result<HostOptions, String> {
        let game = match &self.audio {
            Some(values) => parse_game_capture(values)?,
            None => HostAudioOptions::default().game,
        };
        Ok(HostOptions {
            net: self.net.options(self.headless),
            source: match self.source {
                SourceArg::Synthetic => Source::Synthetic,
                SourceArg::Screen => Source::Screen,
            },
            share: coroom_session::capture::ShareChoice::default(),
            video: LoopbackConfig {
                width: self.width,
                height: self.height,
                fps: self.fps,
                bitrate_kbps: self.bitrate_kbps,
                frames: self.frames.filter(|n| *n > 0),
                yuv444: self.yuv444,
                hdr: self.hdr,
            },
            codec: self.codec.codec(),
            encoder: self.encoder.backend(),
            auto_allow: self.auto_allow.unwrap_or_default(),
            gamepad_to_keys: self.gamepad_to_keys,
            audio: HostAudioOptions {
                game,
                voice: self.voice,
                mic: self.mic.clone(),
                speaker: self.speaker.clone(),
                frame_ms: self.audio_frame_ms,
                noise_suppression: true,
            },
            invites: self.invites.clone(),
        })
    }
}

impl JoinArgs {
    fn options(&self) -> JoinOptions {
        // Con guion y sin `--frames`, se termina con el guion.
        let default = if self.headless && self.input_script.is_none() {
            600
        } else {
            0
        };
        let frames = self.frames.unwrap_or(default);
        JoinOptions {
            net: self.net.options(self.headless),
            code: self
                .code
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_ascii_uppercase(),
            first_invitation: self.first,
            frames: (frames > 0).then_some(frames),
            decoder: self.decoder.backend(),
            input_script: self.input_script.clone(),
            audio: JoinAudioOptions {
                disabled: self.no_audio,
                mute_game: self.mute_game,
                voice: self.voice,
                ptt: self.ptt.clone(),
                mic: self.mic.clone(),
                speaker: self.speaker.clone(),
                noise_suppression: true,
            },
            yuv444: self.yuv444,
            hdr: self.hdr,
        }
    }
}

/// Fuente de vídeo de `coroom probe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceArg {
    /// Patrón sintético en movimiento.
    Synthetic,
    /// Pantalla real (portal en Linux, monitor principal en Windows).
    Screen,
}

#[derive(Debug, Args)]
struct ProbeArgs {
    /// Fuente de vídeo: `synthetic` (por defecto) o `screen`.
    #[arg(
        long,
        value_enum,
        default_value = "synthetic",
        hide_default_value = true,
        hide_possible_values = true,
        value_name = "FUENTE"
    )]
    source: SourceArg,

    /// Ancho en píxeles (por defecto 1920).
    #[arg(long, default_value_t = 1920, hide_default_value = true, value_name = "PÍXELES",
          value_parser = clap::value_parser!(u32).range(16..=3840))]
    width: u32,

    /// Alto en píxeles (por defecto 1080).
    #[arg(long, default_value_t = 1080, hide_default_value = true, value_name = "PÍXELES",
          value_parser = clap::value_parser!(u32).range(16..=2160))]
    height: u32,

    /// Fotogramas por segundo (por defecto 60).
    #[arg(long, default_value_t = 60, hide_default_value = true, value_name = "FPS",
          value_parser = clap::value_parser!(u32).range(1..=240))]
    fps: u32,

    /// Bitrate objetivo en kbit/s (por defecto 10000).
    #[arg(long = "bitrate-kbps", default_value_t = 10_000, hide_default_value = true,
          value_name = "KBIT/S", value_parser = clap::value_parser!(u32).range(100..=500_000))]
    bitrate_kbps: u32,

    /// Fotogramas que presentar; 0 = hasta cerrar (por defecto 0 con ventana y 600 sin ella).
    #[arg(long, value_name = "N")]
    frames: Option<u64>,

    /// Códec: `h264` (por defecto), `hevc` o `av1`.
    #[arg(
        long,
        value_enum,
        default_value = "h264",
        hide_default_value = true,
        hide_possible_values = true,
        value_name = "CÓDEC"
    )]
    codec: CodecArg,

    /// Codificador: `auto` (por defecto), `software`, `vulkan`, `nvenc`, `amf`, `qsv` o `vaapi`.
    #[arg(
        long,
        value_enum,
        default_value = "auto",
        hide_default_value = true,
        hide_possible_values = true,
        value_name = "BACKEND"
    )]
    encoder: EncoderArg,

    /// Decodificador: `auto` (por defecto), `software`, `vulkan`, `nvdec`, `d3d12va`, `d3d11va` o
    /// `vaapi`.
    #[arg(
        long,
        value_enum,
        default_value = "auto",
        hide_default_value = true,
        hide_possible_values = true,
        value_name = "BACKEND"
    )]
    decoder: DecoderArg,

    /// Croma 4:4:4 (solo con codificadores que lo admiten, mira `coroom doctor`).
    #[arg(long)]
    yuv444: bool,

    /// HDR10: captura en coma flotante (la sintética, con el bloque a 1000 nits), 10 bits y
    /// BT.2020 PQ (AV1 por software lo admite).
    #[arg(long)]
    hdr: bool,

    /// Sin ventana: presentación nula y tabla de resultados al terminar.
    #[arg(long)]
    headless: bool,

    /// Imprime el resumen final en JSON.
    #[arg(long)]
    json: bool,
}

impl ProbeArgs {
    fn options(&self) -> ProbeOptions {
        let frames = self.frames.unwrap_or(if self.headless { 600 } else { 0 });
        ProbeOptions {
            source: match self.source {
                SourceArg::Synthetic => Source::Synthetic,
                SourceArg::Screen => Source::Screen,
            },
            config: LoopbackConfig {
                width: self.width,
                height: self.height,
                fps: self.fps,
                bitrate_kbps: self.bitrate_kbps,
                frames: (frames > 0).then_some(frames),
                yuv444: self.yuv444,
                hdr: self.hdr,
            },
            codec: self.codec.codec(),
            encoder: self.encoder.backend(),
            decoder: self.decoder.backend(),
        }
    }
}

/// Logs a stderr (stdout queda para la salida de los comandos). `RUST_LOG` elige el nivel; por
/// defecto, avisos y errores.
fn init_logging() {
    use std::io::IsTerminal;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

fn main() -> ExitCode {
    init_logging();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return report_parse_error(&err),
    };
    match cli.command {
        Some(Command::Doctor) => doctor(),
        Some(Command::Probe(args)) => probe(&args),
        Some(Command::Host(args)) => host(&args),
        Some(Command::Join(args)) => join(&args),
        Some(Command::Login(args)) => print_result(account::login(&LoginOptions {
            account: args.account.options(),
            invitation: args.invitation.clone(),
            user: args.user.clone(),
            display_name: args.display_name.clone(),
            password_stdin: args.password_stdin,
            device_name: args.device_name.clone(),
        })),
        Some(Command::Logout(args)) => print_result(account::logout(&args.options())),
        Some(Command::Whoami(args)) => print_result(account::whoami(&args.options())),
        Some(Command::Friends { action }) => {
            let (args, action) = match action {
                FriendsCommand::List(args) => (args, FriendsAction::List),
                FriendsCommand::Add(friend) => (friend.account, FriendsAction::Add(friend.name)),
                FriendsCommand::Accept(friend) => {
                    (friend.account, FriendsAction::Accept(friend.name))
                }
                FriendsCommand::Remove(friend) => {
                    (friend.account, FriendsAction::Remove(friend.name))
                }
            };
            print_result(account::friends(&args.options(), &action))
        }
        Some(Command::Invite(args)) => print_result(account::invite(
            &args.account.options(),
            &args.name,
            &args.code,
        )),
        None => match ui::run(Launch::App) {
            Ok(_) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Muestra la ayuda o la versión que pidió el usuario, o el error de argumentos en español.
fn print_result(result: Result<String, String>) -> ExitCode {
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn report_parse_error(err: &clap::Error) -> ExitCode {
    match spanish_error(err) {
        None => match err.print() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
        Some(message) => {
            eprintln!("error: {message}\n\nPrueba `coroom --help` para ver las opciones.");
            ExitCode::from(2)
        }
    }
}

/// Traduce los errores de clap, cuyos textos vienen en inglés. `None` si no es un error, sino la
/// ayuda o la versión.
fn spanish_error(err: &clap::Error) -> Option<String> {
    use clap::error::{ContextKind, ErrorKind};

    let context = |kind| {
        err.get(kind)
            .map(|value| value.to_string())
            .unwrap_or_default()
    };
    let message = match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => return None,
        ErrorKind::UnknownArgument => {
            format!(
                "argumento desconocido: {}",
                context(ContextKind::InvalidArg)
            )
        }
        ErrorKind::InvalidSubcommand => {
            format!(
                "comando desconocido: {}",
                context(ContextKind::InvalidSubcommand)
            )
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => format!(
            "valor no válido para {}: {}",
            context(ContextKind::InvalidArg),
            context(ContextKind::InvalidValue)
        ),
        ErrorKind::MissingRequiredArgument => format!(
            "falta un argumento obligatorio: {}",
            context(ContextKind::InvalidArg)
        ),
        ErrorKind::ArgumentConflict => format!(
            "argumentos incompatibles: {}",
            context(ContextKind::InvalidArg)
        ),
        _ => "argumentos no válidos".to_owned(),
    };
    Some(message)
}

fn doctor() -> ExitCode {
    match coroom_desktop::doctor::report() {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Espera a que el usuario pulse Intro (o se cierre la entrada) mientras la prueba corre.
fn wait_for_enter(pipeline: &LoopbackPipeline) {
    eprintln!("Pulsa Intro para terminar.");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        let _ = tx.send(());
    });
    while rx.try_recv().is_err() {
        if pipeline.wait(std::time::Duration::from_millis(100)) {
            return;
        }
    }
}

fn print_summary(summary: &LatencySummary, json: bool) -> ExitCode {
    if json {
        match serde_json::to_string_pretty(summary) {
            Ok(text) => println!("{text}"),
            Err(err) => {
                eprintln!("error: no se pudo serializar el resumen: {err}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!("{summary}");
    }
    ExitCode::SUCCESS
}

fn probe(args: &ProbeArgs) -> ExitCode {
    let options = args.options();
    let mode = if args.headless {
        "sin ventana"
    } else {
        "con ventana"
    };
    eprintln!("Prueba de latencia local ({mode}): {options}");
    let result = if args.headless {
        run_headless(&options, wait_for_enter)
    } else {
        windowed(Launch::Probe(options))
    };
    match result {
        Ok(summary) => print_summary(&summary, args.json),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `probe` y `join` con ventana: la pantalla de sala de la aplicación; al cerrarla, el resumen.
fn windowed(launch: Launch) -> Result<LatencySummary, String> {
    ui::run(launch)?.ok_or_else(|| "la ventana se cerró sin resumen".to_owned())
}

/// Espera a que la sesión termine o el usuario pulse Intro; si la entrada se cierra (por ejemplo,
/// en segundo plano), sigue hasta que termine.
fn wait_for_enter_remote(session: &RemoteSession) {
    eprintln!("Pulsa Intro para terminar.");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if matches!(std::io::stdin().lock().read_line(&mut line), Ok(n) if n > 0) {
            let _ = tx.send(());
        }
    });
    while rx.try_recv().is_err() {
        if session.wait(Duration::from_millis(100)) {
            return;
        }
    }
}

fn end_code(reason: Option<SessionError>) -> ExitCode {
    match reason {
        None => ExitCode::SUCCESS,
        Some(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn host(args: &HostArgs) -> ExitCode {
    let options = match args.options() {
        Ok(options) => options,
        Err(err) => {
            eprintln!("error: {err}\n\nPrueba `coroom host --help` para ver las opciones.");
            return ExitCode::from(2);
        }
    };
    let (identity, place) = match load_identity(options.net.data_dir.as_deref(), args.headless) {
        Ok(found) => found,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "Anfitrión: fuente {}, {}×{} a {} fps, hasta {} kbit/s; audio: {}{}; identidad en \
         {place}.",
        options.source.label(),
        options.video.width,
        options.video.height,
        options.video.fps,
        options.video.bitrate_kbps,
        options.audio.game,
        if options.audio.voice { ", con voz" } else { "" }
    );
    let mut session = match start_host(identity, &options) {
        Ok(session) => session,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    let (joined_tx, joined) = std::sync::mpsc::channel();
    let printer = print_events_forwarding(&session, args.json, Some(joined_tx));
    eprintln!("{CONSOLE_HELP}");
    let account = options.net.session();
    let resolve = |name: &str| -> Result<u64, String> {
        match &account {
            Ok(session) => account::friend_id(session, name),
            Err(_) => Err("las invitaciones necesitan una cuenta (`coroom login`)".to_owned()),
        }
    };
    // La consola sirve también con `--frames`: la sesión termina sola al enviarlos.
    run_host_console(
        &mut session,
        &stdin_lines(),
        &joined,
        options.auto_allow,
        &options.invites,
        &resolve,
    );
    let reason = session.end_reason();
    let guests = session.guests();
    let room = session.room_code();
    session.close();
    let _ = printer.join();
    let summary = session.summary();
    if args.json {
        let guests: Vec<serde_json::Value> = guests
            .iter()
            .map(|guest| {
                serde_json::json!({
                    "guest": guest.guest.0,
                    "fingerprint": guest.fingerprint,
                    "safety_number": guest.safety_number.to_string(),
                    "path": guest.path.map(|path| path.to_string()),
                    "rtt_ms": guest.rtt_ms,
                    "target_kbps": guest.target_kbps,
                    "permissions": coroom_session::remote::permissions_label(&guest.permissions),
                    "input": guest.input,
                })
            })
            .collect();
        let report = serde_json::json!({
            "room": room,
            "guests": guests,
            "latency": summary,
        });
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(err) => eprintln!("error: no se pudo serializar el resumen: {err}"),
        }
    } else {
        println!("{summary}");
    }
    end_code(reason)
}

fn join(args: &JoinArgs) -> ExitCode {
    let mut options = args.options();
    let (identity, place) = match load_identity(options.net.data_dir.as_deref(), args.headless) {
        Ok(found) => found,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    if options.code.is_empty() {
        match wait_for_invitation(&identity, &options.net, options.first_invitation) {
            Ok(code) => options.code = code,
            Err(err) => {
                eprintln!("error: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    let mode = if args.headless {
        "sin ventana"
    } else {
        "con ventana"
    };
    eprintln!(
        "Invitado ({mode}): sala {}; identidad en {place}.",
        options.code
    );
    let result = if args.headless {
        run_join_headless(identity, &options, args.json, |session| {
            wait_for_enter_remote(session)
        })
    } else {
        windowed(Launch::Join {
            identity: Box::new(identity),
            options: Box::new(options),
        })
    };
    match result {
        Ok(summary) => print_summary(&summary, args.json),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_is_in_spanish() {
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("Uso: coroom [OPCIONES] [COMANDO]"), "{help}");
        assert!(help.contains("Comandos"), "{help}");
        assert!(help.contains("Muestra esta ayuda"), "{help}");
        assert!(!help.contains("Usage"), "{help}");
        assert!(!help.contains("Print help"), "{help}");
    }

    #[test]
    fn parse_errors_are_in_spanish() {
        let err = Cli::try_parse_from(["coroom", "jugar"]).expect_err("comando desconocido");
        let message = spanish_error(&err).expect("es un error");
        assert_eq!(message, "comando desconocido: jugar");

        let err = Cli::try_parse_from(["coroom", "--rapido"]).expect_err("argumento desconocido");
        assert_eq!(
            spanish_error(&err).as_deref(),
            Some("argumento desconocido: --rapido")
        );

        let help = Cli::try_parse_from(["coroom", "--help"]).expect_err("la ayuda sale por Err");
        assert_eq!(spanish_error(&help), None);
    }

    #[test]
    fn doctor_subcommand_parses() {
        let cli = Cli::try_parse_from(["coroom", "doctor"]).expect("doctor");
        assert!(matches!(cli.command, Some(Command::Doctor)));
    }

    fn probe_options(args: &[&str]) -> ProbeOptions {
        let cli = Cli::try_parse_from(args).expect("probe");
        let Some(Command::Probe(args)) = cli.command else {
            panic!("se esperaba probe");
        };
        args.options()
    }

    #[test]
    fn probe_defaults_follow_the_contract() {
        let window = probe_options(&["coroom", "probe"]);
        assert_eq!(window.source, Source::Synthetic);
        assert_eq!(
            (window.config.width, window.config.height, window.config.fps),
            (1920, 1080, 60)
        );
        assert_eq!(window.config.bitrate_kbps, 10_000);
        assert_eq!(window.config.frames, None, "con ventana, hasta cerrar");

        let headless = probe_options(&["coroom", "probe", "--headless"]);
        assert_eq!(headless.config.frames, Some(600));

        let custom = probe_options(&[
            "coroom",
            "probe",
            "--source",
            "screen",
            "--width",
            "1280",
            "--height",
            "720",
            "--fps",
            "30",
            "--bitrate-kbps",
            "4000",
            "--frames",
            "0",
            "--headless",
            "--json",
        ]);
        assert_eq!(custom.source, Source::Screen);
        assert_eq!((custom.config.width, custom.config.height), (1280, 720));
        assert_eq!(custom.config.frames, None);
        assert_eq!(
            (custom.codec, custom.encoder, custom.decoder),
            (Codec::H264, None, None),
            "H.264 y backends automáticos por defecto"
        );

        let av1 = probe_options(&[
            "coroom",
            "probe",
            "--codec",
            "av1",
            "--encoder",
            "software",
            "--decoder",
            "d3d12va",
        ]);
        assert_eq!(av1.codec, Codec::Av1);
        assert_eq!(av1.encoder, Some(Backend::Software));
        assert_eq!(av1.decoder, Some(DecoderBackend::D3D12va));
        assert!(Cli::try_parse_from(["coroom", "probe", "--codec", "vp9"]).is_err());
    }

    #[test]
    fn host_and_join_parse_the_contract_options() {
        let cli = Cli::try_parse_from([
            "coroom",
            "host",
            "--server",
            "https://coroom.example.com",
            "--source",
            "synthetic",
            "--force-relay",
            "--trust-fingerprint",
            "ab",
            "--dev-token",
            "change-me",
            "--width",
            "640",
            "--height",
            "360",
            "--fps",
            "30",
            "--headless",
            "--frames",
            "90",
        ])
        .expect("host");
        let Some(Command::Host(args)) = cli.command else {
            panic!("se esperaba host");
        };
        let options = args.options().expect("opciones");
        assert_eq!(options.source, Source::Synthetic);
        assert!(options.net.force_relay && options.net.headless);
        assert_eq!(options.net.dev_token.as_deref(), Some("change-me"));
        assert_eq!((options.video.width, options.video.fps), (640, 30));
        assert_eq!(options.video.frames, Some(90));
        assert_eq!(
            (options.codec, options.encoder),
            (None, None),
            "auto por defecto"
        );

        let cli = Cli::try_parse_from([
            "coroom",
            "host",
            "--server",
            "https://coroom.example.com",
            "--codec",
            "av1",
            "--encoder",
            "nvenc",
        ])
        .expect("host con códec");
        let Some(Command::Host(args)) = cli.command else {
            panic!("se esperaba host");
        };
        let options = args.options().expect("opciones");
        assert_eq!(options.codec, Some(Codec::Av1));
        assert_eq!(options.encoder, Some(Backend::Nvenc));

        let cli = Cli::try_parse_from([
            "coroom",
            "join",
            "--server",
            "https://coroom.example.com",
            "abc234",
            "--headless",
            "--json",
        ])
        .expect("join");
        let Some(Command::Join(args)) = cli.command else {
            panic!("se esperaba join");
        };
        let options = args.options();
        assert_eq!(options.code, "ABC234");
        assert_eq!(options.frames, Some(600), "sin ventana, 600 por defecto");
        assert_eq!(options.decoder, None);
        assert!(!options.net.force_relay);

        // Sin --server se usa el último; sin código, se esperan invitaciones.
        let cli = Cli::try_parse_from(["coroom", "join", "--first"]).expect("join sin código");
        let Some(Command::Join(args)) = cli.command else {
            panic!("se esperaba join");
        };
        let options = args.options();
        assert!(options.code.is_empty() && options.first_invitation);
        assert_eq!(options.net.server, None);
    }

    #[test]
    fn account_commands_parse() {
        let cli = Cli::try_parse_from([
            "coroom",
            "login",
            "--server",
            "https://coroom.example.com",
            "--invitation",
            "ABCD-EFGH-JKLM",
            "--user",
            "ana",
            "--password-stdin",
            "--headless",
        ])
        .expect("login");
        let Some(Command::Login(args)) = cli.command else {
            panic!("se esperaba login");
        };
        assert_eq!(args.invitation.as_deref(), Some("ABCD-EFGH-JKLM"));
        assert!(args.password_stdin && args.account.headless);
        let cli = Cli::try_parse_from([
            "coroom",
            "friends",
            "add",
            "bea",
            "--server",
            "https://coroom.example.com",
        ])
        .expect("friends add");
        assert!(matches!(
            cli.command,
            Some(Command::Friends { action: FriendsCommand::Add(FriendArgs { ref name, .. }) }) if name == "bea"
        ));
        assert!(Cli::try_parse_from(["coroom", "friends", "list"]).is_ok());
        assert!(Cli::try_parse_from(["coroom", "whoami"]).is_ok());
        assert!(
            Cli::try_parse_from(["coroom", "logout", "--server", "https://coroom.example.com"])
                .is_ok()
        );
        let cli =
            Cli::try_parse_from(["coroom", "invite", "bea", "--code", "abc234"]).expect("invite");
        assert!(
            matches!(cli.command, Some(Command::Invite(InviteArgs { ref code, .. })) if code == "abc234")
        );
        assert!(
            Cli::try_parse_from(["coroom", "invite", "bea"]).is_err(),
            "falta --code"
        );
        let cli = Cli::try_parse_from(["coroom", "host", "--invite", "bea", "--invite", "carla"])
            .expect("host con invitaciones");
        let Some(Command::Host(args)) = cli.command else {
            panic!("se esperaba host");
        };
        assert_eq!(args.options().expect("opciones").invites, ["bea", "carla"]);
    }

    #[test]
    fn input_options_parse() {
        let cli = Cli::try_parse_from([
            "coroom",
            "host",
            "--server",
            "https://coroom.example.com",
            "--auto-allow",
            "kbm,pad",
            "--gamepad-to-keys",
        ])
        .expect("host con entrada");
        let Some(Command::Host(args)) = cli.command else {
            panic!("se esperaba host");
        };
        let options = args.options().expect("opciones");
        assert!(options.auto_allow.keyboard_mouse && options.auto_allow.gamepad);
        assert!(options.gamepad_to_keys);
        assert!(
            Cli::try_parse_from([
                "coroom",
                "host",
                "--server",
                "https://coroom.example.com",
                "--auto-allow",
                "todo",
            ])
            .is_err()
        );

        let cli = Cli::try_parse_from([
            "coroom",
            "join",
            "--server",
            "https://coroom.example.com",
            "--headless",
            "--input-script",
            "guion.json",
            "ABC234",
        ])
        .expect("join con guion");
        let Some(Command::Join(args)) = cli.command else {
            panic!("se esperaba join");
        };
        let options = args.options();
        assert_eq!(options.frames, None, "con guion, hasta que termine");
        assert_eq!(
            options.input_script.as_deref(),
            Some(std::path::Path::new("guion.json"))
        );
        // El guion es solo sin ventana.
        assert!(
            Cli::try_parse_from([
                "coroom",
                "join",
                "--server",
                "https://coroom.example.com",
                "--input-script",
                "guion.json",
                "ABC234",
            ])
            .is_err()
        );
    }

    #[test]
    fn audio_options_parse() {
        let host = |extra: &[&str]| {
            let mut args = vec!["coroom", "host", "--server", "https://coroom.example.com"];
            args.extend_from_slice(extra);
            let cli = Cli::try_parse_from(args).expect("host");
            let Some(Command::Host(args)) = cli.command else {
                panic!("se esperaba host");
            };
            args.options()
        };
        let options = host(&[]).expect("por defecto");
        assert_eq!(
            options.audio.game,
            coroom_session::audio::GameCapture::System
        );
        assert!(!options.audio.voice);
        assert_eq!(options.audio.frame_ms, 10);
        let options =
            host(&["--audio", "app", "Juego", "--voice", "--mic", "Micro"]).expect("app y voz");
        assert_eq!(
            options.audio.game,
            coroom_session::audio::GameCapture::App("Juego".into())
        );
        assert!(options.audio.voice);
        assert_eq!(options.audio.mic.as_deref(), Some("Micro"));
        let options = host(&["--audio", "process", "77", "--audio-frame-ms", "20"]).expect("pid");
        assert_eq!(
            options.audio.game,
            coroom_session::audio::GameCapture::Process(77)
        );
        assert_eq!(options.audio.frame_ms, 20);
        assert_eq!(
            host(&["--audio", "none"]).expect("sin audio").audio.game,
            coroom_session::audio::GameCapture::None
        );
        assert!(host(&["--audio", "radio"]).is_err());

        let join = |extra: &[&str]| {
            let mut args = vec!["coroom", "join", "--server", "https://coroom.example.com"];
            args.extend_from_slice(extra);
            args.push("ABC234");
            Cli::try_parse_from(args).map(|cli| match cli.command {
                Some(Command::Join(args)) => args.options(),
                _ => panic!("se esperaba join"),
            })
        };
        let options = join(&[
            "--voice",
            "--ptt",
            "F12",
            "--mute-game",
            "--speaker",
            "Cascos",
        ])
        .expect("join con voz");
        assert!(options.audio.voice && options.audio.mute_game && !options.audio.disabled);
        assert_eq!(options.audio.ptt.as_deref(), Some("F12"));
        assert_eq!(options.audio.speaker.as_deref(), Some("Cascos"));
        assert!(join(&["--ptt", "F12"]).is_err(), "--ptt necesita --voice");
        assert!(join(&["--voice", "--ptt", "NoExiste"]).is_err());
        assert!(join(&["--no-audio"]).expect("sin audio").audio.disabled);
    }

    #[test]
    fn insecure_no_sframe_exists_only_in_debug_builds() {
        let parsed = Cli::try_parse_from([
            "coroom",
            "join",
            "--server",
            "https://coroom.example.com",
            "--insecure-no-sframe",
            "ABC234",
        ]);
        assert_eq!(parsed.is_ok(), cfg!(debug_assertions));
    }

    #[test]
    fn probe_help_is_in_spanish() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("probe")
            .expect("probe")
            .render_help()
            .to_string();
        assert!(help.contains("Uso: coroom probe [OPCIONES]"), "{help}");
        assert!(help.contains("Fuente de vídeo"), "{help}");
        assert!(!help.contains("default"), "{help}");
        assert!(!help.contains("possible values"), "{help}");

        let err = Cli::try_parse_from(["coroom", "probe", "--fps", "0"]).expect_err("fps nulo");
        assert!(
            spanish_error(&err).is_some_and(|message| message.starts_with("valor no válido")),
            "{err}"
        );
    }
}
