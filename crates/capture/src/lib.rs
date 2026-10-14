//! Captura de pantalla de coroom.
//!
//! Define el trait [`Capturer`] que implementan los backends de plataforma, cada uno tras su
//! `feature` de Cargo (`docs/ARQUITECTURA.md` §3): **Windows.Graphics.Capture** en Windows (con regiones sucias en
//! Windows 11 24H2+) y **PipeWire** vía el portal `ScreenCast` en Linux, con DMA-BUF y modificadores
//! explícitos. La captura es por evento: el backend entrega un fotograma cuando el juego presenta.
//! El cursor no se captura dentro de la imagen; viaja como metadatos ([`CursorState`] con la
//! posición y [`CursorShape`] con la imagen de cada forma).
//!
//! Hay tres backends:
//!
//! - [`synthetic::SyntheticCapturer`], siempre compilado: patrón en movimiento a cadencia fija para
//!   la CI y las pruebas A/B ([`FramePixels::Bgra`]; con HDR, [`FramePixels::Rgba16F`] con el
//!   bloque a 1000 nits).
//! - `pipewire` (feature `pipewire`, por defecto; solo actúa en Linux): portal `ScreenCast`. Con
//!   [`GpuFrames::DmaBuf`] negocia DMA-BUF con modificadores explícitos y entrega
//!   [`FramePixels::DmaBuf`] sin tocar la imagen; si el compositor no puede, memoria
//!   compartida copiada a [`FramePixels::Bgra`].
//! - `wgc` (feature `wgc`, por defecto; solo actúa en Windows): Windows.Graphics.Capture. Con
//!   [`GpuFrames::D3D11`] entrega la textura del fotograma ([`FramePixels::D3D11`]); si no,
//!   la copia a una textura de *staging* y la lee en CPU.
//!
//! [`CapturedFrame::cpu_copies`] dice cuántas veces se copió la imagen en memoria de CPU para
//! entregarla (0 en la ruta sin copias). [`screen_capturer`] devuelve el backend de pantalla de la
//! plataforma sin que quien lo usa tenga que distinguir sistemas operativos.

use std::any::Any;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub mod synthetic;

mod counters;
pub use counters::{SourceCounters, source_counters};

#[cfg_attr(
    not(any(
        all(feature = "wgc", target_os = "windows"),
        all(feature = "pipewire", target_os = "linux")
    )),
    allow(dead_code)
)]
mod pacer;
#[cfg_attr(
    not(any(
        all(feature = "wgc", target_os = "windows"),
        all(feature = "pipewire", target_os = "linux")
    )),
    allow(unused_imports)
)]
pub(crate) use pacer::FramePacer;

#[cfg(any(
    all(feature = "pipewire", target_os = "linux"),
    all(feature = "wgc", target_os = "windows")
))]
mod mailbox;

#[cfg(all(feature = "pipewire", target_os = "linux"))]
pub mod pipewire;

#[cfg(all(feature = "wgc", target_os = "windows"))]
pub mod wgc;

/// Qué se captura: un monitor completo o una ventana.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    /// Monitor identificado por el backend.
    Monitor {
        /// Identificador opaco del monitor.
        id: u64,
        /// Nombre legible para mostrar en la interfaz.
        name: String,
    },
    /// Ventana identificada por el backend.
    Window {
        /// Identificador opaco de la ventana.
        id: u64,
        /// Título de la ventana.
        title: String,
        /// Clave de la aplicación dueña de la ventana, la misma que usa el audio por aplicación
        /// (`coroom_audio::PlayingApp::key`), para marcar su audio. `None` si no se sabe.
        app: Option<String>,
    },
}

/// Qué comparte el anfitrión.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShareKind {
    /// Un monitor entero.
    #[default]
    Screen,
    /// Una sola ventana (si no se puede capturar, el backend pasa a su monitor).
    App,
}

/// Lo que se comparte: la clase y, si la interfaz lo eligió, el destino.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShareChoice {
    /// Pantalla o aplicación.
    pub kind: ShareKind,
    /// Destino elegido de [`share_targets`] o de [`pick_share`]; `None` si lo elige el sistema al
    /// empezar (el diálogo del portal) o, si [`share_targets`] da destinos, el primero.
    pub target: Option<CaptureTarget>,
    /// Token del portal de [`pick_share`]: al empezar, el portal repite esa elección sin volver a
    /// preguntar (si el escritorio no puede, vuelve a mostrar su selector).
    pub restore_token: Option<String>,
}

/// Lo que eligió el usuario en el selector del sistema antes de abrir la sala.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedShare {
    /// Monitor o ventana, con lo que el sistema dice de él (en Wayland, solo el tamaño).
    pub target: CaptureTarget,
    /// Token para repetir la elección al empezar.
    pub restore_token: Option<String>,
}

/// Abre el selector del sistema para elegir ya qué compartir (el portal `ScreenCast` en Linux).
/// `Ok(None)` donde la interfaz lista los destinos ella misma ([`share_targets`]).
pub fn pick_share(kind: ShareKind) -> Result<Option<PickedShare>, CaptureError> {
    platform::pick_share(kind)
}

/// Destinos que la interfaz puede ofrecer para `kind`. Vacío si los elige el sistema al empezar
/// (el portal en Linux muestra su propio diálogo).
pub fn share_targets(kind: ShareKind) -> Result<Vec<CaptureTarget>, CaptureError> {
    platform::share_targets(kind)
}

/// Capturador de lo elegido en `choice`, con la sesión del portal compartida con la entrada donde
/// la hay (como [`screen_capturer_sharing_input`]).
pub fn share_capturer(
    choice: &ShareChoice,
) -> Result<(Box<dyn Capturer>, Option<RemoteDesktopShare>), CaptureError> {
    platform::share_capturer(choice)
}

/// Capturador con un destino fijo: [`Capturer::targets`] devuelve solo ese, así la sesión (que
/// captura el primero) captura lo que eligió la interfaz.
struct Chosen {
    inner: Box<dyn Capturer>,
    target: CaptureTarget,
}

impl Capturer for Chosen {
    fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError> {
        Ok(vec![self.target.clone()])
    }

    fn start(&mut self, target: &CaptureTarget) -> Result<(), CaptureError> {
        self.inner.start(target)
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        self.inner.next_frame()
    }

    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        self.inner.next_frame_timeout(timeout)
    }

    fn stop(&mut self) {
        self.inner.stop();
    }

    fn set_gpu_frames(&mut self, frames: GpuFrames) -> GpuFrames {
        self.inner.set_gpu_frames(frames)
    }

    fn set_options(&mut self, options: CaptureOptions) -> CaptureOptions {
        self.inner.set_options(options)
    }
}

/// Envuelve `inner` para que capture `target` si se eligió uno.
#[cfg_attr(
    not(all(feature = "wgc", target_os = "windows")),
    expect(dead_code, reason = "solo Windows ofrece destinos en la interfaz")
)]
fn chosen(inner: Box<dyn Capturer>, target: Option<&CaptureTarget>) -> Box<dyn Capturer> {
    match target {
        Some(target) => Box::new(Chosen {
            inner,
            target: target.clone(),
        }),
        None => inner,
    }
}

/// Forma y posición del cursor, que el invitado dibuja en local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    /// Posición horizontal en píxeles del fotograma.
    pub x: i32,
    /// Posición vertical en píxeles del fotograma.
    pub y: i32,
    /// Si el cursor es visible.
    pub visible: bool,
    /// Identificador de la forma actual; cambia cuando cambia la imagen del cursor.
    pub shape_id: u64,
}

/// Lado máximo de una forma de cursor que entregan los backends (las mayores se recortan).
pub const CURSOR_MAX_SIDE: u32 = 256;

/// Imagen de una forma de cursor: RGBA de 8 bits sin premultiplicar, fila a fila, con su punto
/// activo (el píxel que señala la posición de [`CursorState`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    /// Identificador ([`CursorState::shape_id`]).
    pub shape_id: u64,
    /// Ancho en píxeles (como mucho [`CURSOR_MAX_SIDE`]).
    pub width: u32,
    /// Alto en píxeles (como mucho [`CURSOR_MAX_SIDE`]).
    pub height: u32,
    /// Punto activo, horizontal.
    pub hotspot_x: u32,
    /// Punto activo, vertical.
    pub hotspot_y: u32,
    /// `width × height × 4` bytes R, G, B, A.
    pub rgba: Vec<u8>,
}

impl CursorShape {
    /// Comprueba que el tamaño, el punto activo y los datos cuadran.
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= CURSOR_MAX_SIDE
            && self.height <= CURSOR_MAX_SIDE
            && self.hotspot_x < self.width
            && self.hotspot_y < self.height
            && self.rgba.len() == self.width as usize * self.height as usize * 4
    }
}

/// Algo que mantiene viva la imagen de GPU (el búfer de PipeWire, el fotograma de WGC); al soltar
/// el último `Arc` vuelve a su productor.
pub type Release = Arc<dyn Any + Send + Sync>;

/// Plano de un DMA-BUF.
#[cfg(unix)]
#[derive(Debug)]
pub struct DmaBufPlane {
    /// Descriptor propio (un `dup` del de PipeWire): se cierra al soltarlo.
    pub fd: std::os::fd::OwnedFd,
    /// Desplazamiento del plano en el objeto, en bytes.
    pub offset: u32,
    /// Bytes por fila.
    pub stride: u32,
}

/// Imagen DMA-BUF de la captura.
#[cfg(unix)]
pub struct DmaBufFrame {
    /// Planos (uno en los formatos RGB de 32 bits).
    pub planes: Vec<DmaBufPlane>,
    /// Modificador DRM negociado.
    pub modifier: u64,
    /// Formato DRM (`XR24`, `AR24`, `XB24` o `AB24`).
    pub fourcc: u32,
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
    /// El búfer de PipeWire, que no vuelve al compositor mientras viva (así no se sobrescribe la
    /// imagen mientras se codifica).
    pub release: Option<Release>,
}

#[cfg(unix)]
impl fmt::Debug for DmaBufFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaBufFrame")
            .field("planes", &self.planes)
            .field("modifier", &format_args!("{:#018x}", self.modifier))
            .field("fourcc", &format_args!("{:#010x}", self.fourcc))
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Dos imágenes DMA-BUF son iguales si apuntan a los mismos descriptores con la misma disposición.
#[cfg(unix)]
impl PartialEq for DmaBufFrame {
    fn eq(&self, other: &Self) -> bool {
        use std::os::fd::AsRawFd;
        let planes = |frame: &Self| -> Vec<(i32, u32, u32)> {
            frame
                .planes
                .iter()
                .map(|p| (p.fd.as_raw_fd(), p.offset, p.stride))
                .collect()
        };
        planes(self) == planes(other)
            && (self.modifier, self.fourcc, self.width, self.height)
                == (other.modifier, other.fourcc, other.width, other.height)
    }
}

#[cfg(unix)]
impl Eq for DmaBufFrame {}

/// Textura D3D11 de Windows.Graphics.Capture, `DXGI_FORMAT_B8G8R8A8_UNORM`.
#[cfg(windows)]
pub struct D3D11Frame {
    /// Textura del fotograma.
    pub texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    /// Subrecurso con la imagen.
    pub subresource: u32,
    /// Dispositivo de la captura (el codificador usa el mismo, sin compartir texturas).
    pub device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    /// El `Direct3D11CaptureFrame`: la textura no vuelve al pool de WGC mientras viva.
    pub release: Option<Release>,
}

#[cfg(windows)]
impl fmt::Debug for D3D11Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D11Frame")
            .field("subresource", &self.subresource)
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
impl PartialEq for D3D11Frame {
    fn eq(&self, other: &Self) -> bool {
        use windows::core::Interface;
        self.texture.as_raw() == other.texture.as_raw() && self.subresource == other.subresource
    }
}

#[cfg(windows)]
impl Eq for D3D11Frame {}

/// Píxeles de un fotograma capturado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramePixels {
    /// Memoria de CPU, 4 bytes por píxel en orden B, G, R, A (o X: el cuarto byte se ignora).
    Bgra {
        /// Filas de `stride` bytes; cada una con `width × 4` bytes útiles al principio.
        data: Vec<u8>,
        /// Bytes por fila.
        stride: u32,
    },
    /// Memoria de CPU en HDR: RGBA de medio flotante (`f16` *little-endian*, 8 bytes
    /// por píxel) en scRGB lineal, 1,0 = 80 nits (WGC `R16G16B16A16Float`, PipeWire `RGBA_F16`).
    Rgba16F {
        /// Filas de `stride` bytes; cada una con `width × 8` bytes útiles al principio.
        data: Vec<u8>,
        /// Bytes por fila.
        stride: u32,
    },
    /// DMA-BUF de PipeWire (Linux).
    #[cfg(unix)]
    DmaBuf(Arc<DmaBufFrame>),
    /// Textura D3D11 de WGC (Windows).
    #[cfg(windows)]
    D3D11(Arc<D3D11Frame>),
}

impl FramePixels {
    /// Si la imagen está en la GPU.
    pub fn is_gpu(&self) -> bool {
        !matches!(self, FramePixels::Bgra { .. } | FramePixels::Rgba16F { .. })
    }
}

/// Lo que se le pide a la captura además de las imágenes de GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureOptions {
    /// Fotogramas por segundo que se quieren como mucho (PipeWire lo propone al compositor como
    /// `framerate` y `maxFramerate`; WGC, como intervalo mínimo entre fotogramas en Windows 11
    /// 24H2).
    pub max_fps: u32,
    /// Captura HDR en coma flotante ([`FramePixels::Rgba16F`]) si el backend y la pantalla la
    /// dan; si no, la de siempre.
    pub hdr: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            max_fps: 60,
            hdr: false,
        }
    }
}

/// Imágenes de GPU que se le piden a la captura (las que importa el codificador sin copia).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GpuFrames {
    /// Solo memoria de CPU.
    #[default]
    Off,
    /// DMA-BUF con estos modificadores DRM, en orden de preferencia (Linux, PipeWire).
    DmaBuf {
        /// Modificadores que acepta el codificador para `XRGB8888`/`ARGB8888`.
        modifiers: Vec<u64>,
    },
    /// Texturas D3D11 (Windows, WGC).
    D3D11,
}

/// Fotograma capturado: metadatos e imagen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame {
    /// Identificador creciente del fotograma; acompaña a sus marcas de tiempo por etapa.
    pub frame_id: u64,
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
    /// Momento de la captura, primera marca de tiempo del pipeline.
    pub captured_at: Instant,
    /// Cursor en el momento de la captura, si el backend lo conoce.
    pub cursor: Option<CursorState>,
    /// Forma actual del cursor, si el backend la conoce (la misma `Arc` mientras no cambie).
    pub cursor_shape: Option<Arc<CursorShape>>,
    /// Imagen capturada.
    pub pixels: FramePixels,
    /// Copias de la imagen en memoria de CPU para entregarla (de la memoria compartida o de la
    /// textura de *staging*; 0 con imágenes de GPU o generadas en CPU).
    pub cpu_copies: u32,
}

/// Error de un backend de captura.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// El usuario rechazó el permiso de captura (por ejemplo, en el portal `ScreenCast`).
    PermissionDenied,
    /// El objetivo ya no existe (monitor desconectado, ventana cerrada).
    TargetLost,
    /// La plataforma o el compositor no admite este backend.
    Unsupported(String),
    /// Error interno del backend.
    Backend(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::PermissionDenied => f.write_str("permiso de captura denegado"),
            CaptureError::TargetLost => f.write_str("el monitor o la ventana ya no existe"),
            CaptureError::Unsupported(what) => write!(f, "captura no admitida: {what}"),
            CaptureError::Backend(what) => write!(f, "error del backend de captura: {what}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// Backend de captura de pantalla compilado en este binario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Patrón sintético, siempre disponible.
    Synthetic,
    /// PipeWire vía el portal `ScreenCast` (Linux).
    PipeWire,
    /// Windows.Graphics.Capture (Windows).
    Wgc,
}

impl Backend {
    /// Nombre legible del backend.
    pub const fn name(self) -> &'static str {
        match self {
            Backend::Synthetic => "sintético",
            Backend::PipeWire => "PipeWire (portal ScreenCast)",
            Backend::Wgc => "Windows.Graphics.Capture",
        }
    }
}

/// Backends compilados en este binario, empezando por el sintético.
pub fn compiled_backends() -> Vec<Backend> {
    let mut backends = vec![Backend::Synthetic];
    backends.extend(platform::BACKEND);
    backends
}

/// Crea el capturador de pantalla de la plataforma (portal en Linux, WGC en Windows).
pub fn screen_capturer() -> Result<Box<dyn Capturer>, CaptureError> {
    platform::screen_capturer()
}

/// Socket del protocolo EI (libei) de la sesión del portal `RemoteDesktop` que abre la captura
/// (solo existe en Linux).
#[derive(Debug)]
pub struct EisSocket {
    /// Socket Unix.
    #[cfg(unix)]
    pub fd: std::os::fd::OwnedFd,
    /// Qué eligió el usuario en el selector del portal.
    pub area: SharedArea,
}

/// Zona del escritorio que se transmite, en coordenadas lógicas del compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharedArea {
    /// El portal no lo dijo.
    #[default]
    Unknown,
    /// Un monitor con la esquina superior izquierda en `(x, y)`.
    Monitor {
        /// Esquina superior izquierda.
        x: i32,
        /// Esquina superior izquierda.
        y: i32,
    },
    /// Una ventana (Wayland no dice dónde está).
    Window {
        /// Ancho lógico.
        width: u32,
        /// Alto lógico.
        height: u32,
    },
}

/// Entrega del socket de EI de la sesión `RemoteDesktop`, cuando el usuario acepta el portal.
#[derive(Debug)]
pub struct RemoteDesktopShare {
    rx: std::sync::mpsc::Receiver<Result<EisSocket, String>>,
}

impl RemoteDesktopShare {
    /// Espera el socket como mucho `timeout` (la captura se abre al arrancar la sesión).
    pub fn wait(self, timeout: Duration) -> Result<EisSocket, String> {
        match self.rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err("el portal RemoteDesktop no respondió a tiempo".to_owned())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("la captura terminó sin abrir la sesión del portal".to_owned())
            }
        }
    }
}

/// Capturador de pantalla que comparte su sesión del portal con la entrada del anfitrión
/// (`RemoteDesktop` + `ScreenCast` en Linux); donde no hay portal (Windows) no hay nada que
/// compartir y devuelve `None`.
pub fn screen_capturer_sharing_input()
-> Result<(Box<dyn Capturer>, Option<RemoteDesktopShare>), CaptureError> {
    platform::screen_capturer_sharing_input()
}

/// Comprueba si el servicio de captura de la plataforma responde, sin pedir permiso al usuario.
///
/// En Linux pregunta por la versión de la interfaz `ScreenCast` del portal; en Windows comprueba
/// que `GraphicsCaptureSession` está admitido. `Ok(None)` si no hay backend de pantalla compilado.
pub fn probe_screen_capture() -> Result<Option<String>, CaptureError> {
    platform::probe()
}

#[cfg(all(feature = "pipewire", target_os = "linux"))]
mod platform {
    use super::{Backend, CaptureError, Capturer};

    pub const BACKEND: Option<Backend> = Some(Backend::PipeWire);

    pub fn screen_capturer() -> Result<Box<dyn Capturer>, CaptureError> {
        Ok(Box::new(crate::pipewire::PipeWireCapturer::new()))
    }

    pub fn screen_capturer_sharing_input()
    -> Result<(Box<dyn Capturer>, Option<super::RemoteDesktopShare>), CaptureError> {
        let mut capturer = crate::pipewire::PipeWireCapturer::new();
        let share = capturer.share_remote_desktop();
        Ok((Box::new(capturer), Some(share)))
    }

    pub fn share_targets(
        _kind: super::ShareKind,
    ) -> Result<Vec<super::CaptureTarget>, CaptureError> {
        Ok(Vec::new())
    }

    pub fn share_capturer(
        choice: &super::ShareChoice,
    ) -> Result<(Box<dyn Capturer>, Option<super::RemoteDesktopShare>), CaptureError> {
        let mut capturer = crate::pipewire::PipeWireCapturer::new()
            .with_kind(choice.kind)
            .restoring(choice.restore_token.clone());
        let share = capturer.share_remote_desktop();
        Ok((Box::new(capturer), Some(share)))
    }

    pub fn pick_share(kind: super::ShareKind) -> Result<Option<super::PickedShare>, CaptureError> {
        crate::pipewire::pick(kind).map(Some)
    }

    pub fn probe() -> Result<Option<String>, CaptureError> {
        crate::pipewire::portal_status().map(Some)
    }
}

#[cfg(all(feature = "wgc", target_os = "windows"))]
mod platform {
    use super::{Backend, CaptureError, Capturer};

    pub const BACKEND: Option<Backend> = Some(Backend::Wgc);

    pub fn screen_capturer() -> Result<Box<dyn Capturer>, CaptureError> {
        Ok(Box::new(crate::wgc::WgcCapturer::new()))
    }

    pub fn screen_capturer_sharing_input()
    -> Result<(Box<dyn Capturer>, Option<super::RemoteDesktopShare>), CaptureError> {
        Ok((screen_capturer()?, None))
    }

    pub fn share_targets(
        kind: super::ShareKind,
    ) -> Result<Vec<super::CaptureTarget>, CaptureError> {
        let capturer = crate::wgc::WgcCapturer::new();
        match kind {
            super::ShareKind::Screen => capturer.targets(),
            super::ShareKind::App => crate::wgc::list_windows(),
        }
    }

    pub fn share_capturer(
        choice: &super::ShareChoice,
    ) -> Result<(Box<dyn Capturer>, Option<super::RemoteDesktopShare>), CaptureError> {
        let capturer: Box<dyn Capturer> = Box::new(crate::wgc::WgcCapturer::new());
        Ok((super::chosen(capturer, choice.target.as_ref()), None))
    }

    pub fn pick_share(_kind: super::ShareKind) -> Result<Option<super::PickedShare>, CaptureError> {
        Ok(None)
    }

    pub fn probe() -> Result<Option<String>, CaptureError> {
        crate::wgc::support_status().map(Some)
    }
}

#[cfg(not(any(
    all(feature = "pipewire", target_os = "linux"),
    all(feature = "wgc", target_os = "windows")
)))]
mod platform {
    use super::{Backend, CaptureError, Capturer};

    pub const BACKEND: Option<Backend> = None;

    pub fn screen_capturer() -> Result<Box<dyn Capturer>, CaptureError> {
        Err(CaptureError::Unsupported(
            "este binario no incluye captura de pantalla para este sistema".into(),
        ))
    }

    pub fn screen_capturer_sharing_input()
    -> Result<(Box<dyn Capturer>, Option<super::RemoteDesktopShare>), CaptureError> {
        Ok((screen_capturer()?, None))
    }

    pub fn share_targets(
        _kind: super::ShareKind,
    ) -> Result<Vec<super::CaptureTarget>, CaptureError> {
        Ok(Vec::new())
    }

    pub fn share_capturer(
        _choice: &super::ShareChoice,
    ) -> Result<(Box<dyn Capturer>, Option<super::RemoteDesktopShare>), CaptureError> {
        Ok((screen_capturer()?, None))
    }

    pub fn pick_share(_kind: super::ShareKind) -> Result<Option<super::PickedShare>, CaptureError> {
        Ok(None)
    }

    pub fn probe() -> Result<Option<String>, CaptureError> {
        Ok(None)
    }
}

/// Fuente de fotogramas de pantalla.
///
/// Es `Send` porque cada backend vive en su propio hilo de captura.
pub trait Capturer: Send {
    /// Monitores y ventanas que se pueden capturar.
    fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError>;

    /// Empieza a capturar `target`. Puede pedir permiso al usuario.
    fn start(&mut self, target: &CaptureTarget) -> Result<(), CaptureError>;

    /// Espera al siguiente fotograma presentado por el juego.
    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError>;

    /// Como [`Capturer::next_frame`], pero espera como mucho `timeout`; `Ok(None)` si no llegó
    /// nada (la captura por evento no entrega fotogramas mientras la pantalla no cambia).
    ///
    /// La implementación por defecto delega en [`Capturer::next_frame`], que ya vuelve a tiempo en
    /// las fuentes con cadencia fija; los backends por evento la sobrescriben.
    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        let _ = timeout;
        self.next_frame().map(Some)
    }

    /// Detiene la captura y libera los recursos del backend.
    fn stop(&mut self);

    /// Pide imágenes de GPU en las próximas capturas que se arranquen; los backends que
    /// no las tienen lo ignoran. Devuelve lo que el backend va a intentar.
    fn set_gpu_frames(&mut self, frames: GpuFrames) -> GpuFrames {
        let _ = frames;
        GpuFrames::Off
    }

    /// Opciones de las próximas capturas que se arranquen. Devuelve lo que el backend
    /// va a intentar: por defecto, sin HDR.
    fn set_options(&mut self, options: CaptureOptions) -> CaptureOptions {
        CaptureOptions {
            hdr: false,
            ..options
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullCapturer;

    impl Capturer for NullCapturer {
        fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError> {
            Ok(Vec::new())
        }

        fn start(&mut self, _target: &CaptureTarget) -> Result<(), CaptureError> {
            Err(CaptureError::Unsupported("prueba".into()))
        }

        fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
            Err(CaptureError::TargetLost)
        }

        fn stop(&mut self) {}
    }

    fn assert_send<T: Send + ?Sized>() {}

    #[test]
    fn capturer_is_send_and_object_safe() {
        assert_send::<dyn Capturer>();
        let mut capturer: Box<dyn Capturer> = Box::new(NullCapturer);
        assert_eq!(capturer.targets(), Ok(Vec::new()));
        assert_eq!(capturer.next_frame(), Err(CaptureError::TargetLost));
        assert_eq!(capturer.set_gpu_frames(GpuFrames::D3D11), GpuFrames::Off);
        let asked = CaptureOptions {
            max_fps: 120,
            hdr: true,
        };
        assert_eq!(
            capturer.set_options(asked),
            CaptureOptions {
                max_fps: 120,
                hdr: false
            },
            "sin HDR por defecto"
        );
        capturer.stop();
    }

    #[cfg(unix)]
    #[test]
    fn dmabuf_frames_compare_by_descriptor() {
        let fd = |path: &str| -> std::os::fd::OwnedFd {
            std::fs::File::open(path).expect("fichero").into()
        };
        let frame = |fd| DmaBufFrame {
            planes: vec![DmaBufPlane {
                fd,
                offset: 0,
                stride: 64,
            }],
            modifier: 0,
            fourcc: 0x3432_5258,
            width: 16,
            height: 16,
            release: None,
        };
        let a = Arc::new(frame(fd("/dev/null")));
        let b = Arc::new(frame(fd("/dev/null")));
        assert_eq!(
            FramePixels::DmaBuf(Arc::clone(&a)),
            FramePixels::DmaBuf(Arc::clone(&a))
        );
        assert_ne!(
            FramePixels::DmaBuf(a),
            FramePixels::DmaBuf(b),
            "otro descriptor"
        );
        assert!(
            !FramePixels::Bgra {
                data: Vec::new(),
                stride: 0
            }
            .is_gpu()
        );
    }
}
