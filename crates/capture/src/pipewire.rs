//! Captura en Linux con el portal `ScreenCast` (`ashpd`) y PipeWire, en DMA-BUF o en memoria
//! compartida.
//!
//! - El portal se habla desde un hilo `portal` con un runtime `tokio` de un solo hilo, que mantiene
//!   viva la sesión (y su conexión D-Bus) mientras dura la captura. Tipos `Monitor | Window`,
//!   cursor como metadatos si el portal lo anuncia (si no, oculto) y `persist_mode` con token de
//!   restauración opcional ([`PipeWireCapturer::with_restore_token`]).
//! - **Sesión compartida con `RemoteDesktop`** ([`PipeWireCapturer::share_remote_desktop`]):
//!   la sesión se crea con el portal `RemoteDesktop` (teclado y puntero, `persist_mode` y token de
//!   restauración en `SelectDevices`), las fuentes de pantalla se eligen con `ScreenCast` sobre esa
//!   **misma sesión** y `RemoteDesktop.Start` muestra un solo diálogo para las dos cosas. Después
//!   se abre el nodo de PipeWire como siempre y `ConnectToEIS` da el socket de libei para la
//!   inyección del anfitrión ([`RemoteDesktopShare`]). Si el escritorio no tiene portal
//!   `RemoteDesktop`, la captura sigue sola con `ScreenCast` y la entrega de EI falla con el motivo.
//! - Con el fd y el id de nodo, un hilo `pipewire` ejecuta `pipewire::main_loop` con un `Stream`
//!   conectado al nodo y metadatos de cursor (`SPA_META_Cursor`, con el mapa de bits de la forma
//!   cuando cambia, que se convierte a RGBA).
//! - **Formatos** (`EnumFormat`), en orden de preferencia: con [`GpuFrames::DmaBuf`],
//!   `video/raw` BGRx o BGRA con la propiedad `SPA_FORMAT_VIDEO_modifier` marcada
//!   `MANDATORY | DONT_FIXATE` y la lista de modificadores que importa el codificador (el
//!   compositor, que reserva los búferes, fija uno, como manda la negociación de DMA-BUF de
//!   PipeWire); después, siempre, BGRx, BGRA, RGBx o RGBA sin modificador, que el compositor da en
//!   memoria compartida.
//! - **Búferes**: si el formato negociado lleva modificador, `SPA_DATA_DmaBuf`; si no,
//!   `MemPtr | MemFd`.
//! - **`process`**: con DMA-BUF, el búfer **no** se devuelve a PipeWire al salir del callback: se
//!   duplican sus descriptores (`dup`) en un [`FramePixels::DmaBuf`] cuyo `release` lo devuelve al
//!   compositor (por un canal de `pipewire` hacia este hilo) cuando el codificador termina con él,
//!   así la imagen no se sobrescribe mientras se codifica. Se piden de 4 a 16 búferes para que el
//!   compositor no se quede sin ellos. En memoria compartida, el callback copia la imagen a BGRA
//!   (una copia a CPU) y devuelve el búfer al momento. El fotograma va a un buzón de un solo hueco:
//!   gana el último.
//!
//! El `unsafe` de este módulo (marcado con `// SAFETY:`) es el de quedarse con un `pw_buffer` más
//! allá del callback (`dequeue_raw_buffer`/`pw_stream_queue_buffer`) y el de duplicar sus
//! descriptores de DMA-BUF; la API segura de `pipewire` devuelve siempre el búfer al soltarlo.

// Backend nativo de PipeWire (ver `CLAUDE.md`): retener búferes y duplicar descriptores.
#![allow(unsafe_code)]

use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use pipewire as pw;
use pw::spa;
use pw::spa::buffer::meta::{MetaBitmap, MetaCursor};
use pw::spa::buffer::{ChunkFlags, Data, DataType};
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, PropertyFlags, Value};
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Id};

use crate::mailbox::Mailbox;
use crate::{
    CURSOR_MAX_SIDE, CaptureError, CaptureOptions, CaptureTarget, CapturedFrame, Capturer,
    CursorShape, CursorState, DmaBufFrame, DmaBufPlane, EisSocket, FramePixels, GpuFrames,
    RemoteDesktopShare,
};

/// Nombre del único objetivo: el selector lo muestra el portal.
pub const TARGET_NAME: &str = "Lo que elija el portal";

/// Tiempo máximo para comprobar el portal en `coroom doctor`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Espera máxima de las llamadas al portal que no esperan al usuario (conectar, leer versiones).
const PORTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Espera máxima de una sesión del portal entera, con el diálogo en el que el usuario elige qué
/// compartir. Si se agota, el diálogo no llegó a mostrarse o nadie lo contestó.
const DIALOG_TIMEOUT: Duration = Duration::from_secs(180);

/// Lado máximo del mapa de bits del cursor que se reserva en los metadatos.
const CURSOR_BITMAP_SIDE: usize = 64;

fn backend(what: impl std::fmt::Display) -> CaptureError {
    CaptureError::Backend(what.to_string())
}

fn portal_error(err: ashpd::Error) -> CaptureError {
    match err {
        ashpd::Error::Response(ResponseError::Cancelled) => CaptureError::PermissionDenied,
        ashpd::Error::PortalNotFound(_) => {
            CaptureError::Unsupported("no hay portal ScreenCast en este escritorio".into())
        }
        ashpd::Error::Zbus(err) => CaptureError::Unsupported(format!(
            "no se pudo hablar con el portal por el bus de sesión D-Bus ({err})"
        )),
        other => CaptureError::Backend(format!("portal ScreenCast: {other}")),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, CaptureError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| backend(format!("no se pudo crear el runtime del portal: {err}")))
}

/// Versión del portal `ScreenCast` y tipos de fuente, sin abrir sesión ni pedir permiso.
pub fn portal_status() -> Result<String, CaptureError> {
    runtime()?.block_on(async {
        let probe = async {
            let proxy = Screencast::new().await.map_err(portal_error)?;
            let sources = proxy.available_source_types().await.unwrap_or_default();
            let mut kinds = Vec::new();
            if sources.contains(SourceType::Monitor) {
                kinds.push("monitores");
            }
            if sources.contains(SourceType::Window) {
                kinds.push("ventanas");
            }
            let kinds = if kinds.is_empty() {
                "sin tipos anunciados".to_owned()
            } else {
                kinds.join(" y ")
            };
            Ok(format!(
                "portal ScreenCast disponible (versión {}, {kinds})",
                proxy.version()
            ))
        };
        tokio::time::timeout(PROBE_TIMEOUT, probe)
            .await
            .unwrap_or_else(|_| {
                Err(CaptureError::Unsupported(
                    "el portal ScreenCast no respondió a tiempo".into(),
                ))
            })
    })
}

/// Lo que devuelve el portal al empezar.
struct PortalStream {
    fd: OwnedFd,
    node_id: u32,
    restore_token: Option<String>,
}

/// Socket de EI que se entrega a la inyección (o por qué no lo hay).
type EisSender = std::sync::mpsc::Sender<Result<EisSocket, String>>;

/// Sesiones del portal que hay que mantener vivas mientras dura la captura.
enum PortalSession {
    ScreenCast(Session<Screencast>),
    RemoteDesktop(Session<RemoteDesktop>),
}

impl PortalSession {
    async fn close(&self) {
        let _ = match self {
            PortalSession::ScreenCast(session) => session.close().await,
            PortalSession::RemoteDesktop(session) => session.close().await,
        };
    }
}

fn remote_desktop_error(err: ashpd::Error) -> String {
    match err {
        ashpd::Error::Response(ResponseError::Cancelled) => {
            "el usuario no concedió el control remoto (portal RemoteDesktop)".to_owned()
        }
        ashpd::Error::PortalNotFound(_) => "no hay portal RemoteDesktop en este escritorio".into(),
        ashpd::Error::RequiresVersion(need, have) => format!(
            "el portal RemoteDesktop es la versión {have} y libei necesita la {need} \
             (xdg-desktop-portal 1.17 o posterior)"
        ),
        other => format!("portal RemoteDesktop: {other}"),
    }
}

/// Tipos de fuente que ofrece el diálogo del portal.
type Sources = ashpd::enumflags2::BitFlags<SourceType>;

/// Fuentes del diálogo para lo que se comparte.
fn sources(kind: crate::ShareKind) -> Sources {
    match kind {
        crate::ShareKind::Screen => SourceType::Monitor.into(),
        crate::ShareKind::App => SourceType::Window.into(),
    }
}

/// Sesión `ScreenCast` sola (sin entrada).
async fn open_screencast(
    proxy: &Screencast,
    cursor_mode: CursorMode,
    restore_token: Option<&str>,
    sources: Sources,
) -> Result<(u32, Option<String>, OwnedFd, PortalSession), CaptureError> {
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(portal_error)?;
    let mut options = SelectSourcesOptions::default()
        .set_cursor_mode(cursor_mode)
        .set_sources(sources)
        .set_multiple(false);
    // `persist_mode` y el token existen desde la versión 4 de la interfaz.
    if proxy.version() >= 4 {
        options = options
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(restore_token);
    }
    proxy
        .select_sources(&session, options)
        .await
        .map_err(portal_error)?
        .response()
        .map_err(portal_error)?;
    let streams = proxy
        .start(&session, None, Default::default())
        .await
        .map_err(portal_error)?
        .response()
        .map_err(portal_error)?;
    let Some(stream) = streams.streams().first() else {
        return Err(CaptureError::PermissionDenied);
    };
    let node_id = stream.pipe_wire_node_id();
    let restore_token = streams.restore_token().map(str::to_owned);
    let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(portal_error)?;
    Ok((
        node_id,
        restore_token,
        fd,
        PortalSession::ScreenCast(session),
    ))
}

/// [`open_screencast`] con [`DIALOG_TIMEOUT`] de límite.
async fn screencast_with_timeout(
    proxy: &Screencast,
    cursor_mode: CursorMode,
    restore_token: Option<&str>,
    sources: Sources,
) -> Result<(u32, Option<String>, OwnedFd, PortalSession), CaptureError> {
    tracing::debug!("portal ScreenCast: abriendo sesión y diálogo");
    tokio::time::timeout(
        DIALOG_TIMEOUT,
        open_screencast(proxy, cursor_mode, restore_token, sources),
    )
    .await
    .unwrap_or_else(|_| {
        Err(backend(format!(
            "el diálogo del portal ScreenCast no terminó en {} s (¿no se mostró?)",
            DIALOG_TIMEOUT.as_secs()
        )))
    })
}

/// Una sola sesión `RemoteDesktop` con las fuentes de `ScreenCast` encima: un diálogo, un token.
/// Devuelve también el socket de EI (o por qué no se pudo abrir).
async fn open_shared(
    screencast: &Screencast,
    cursor_mode: CursorMode,
    restore_token: Option<&str>,
    sources: Sources,
) -> Result<
    (
        u32,
        Option<String>,
        OwnedFd,
        PortalSession,
        RemoteDesktop,
        Result<OwnedFd, String>,
    ),
    ashpd::Error,
> {
    let remote = RemoteDesktop::new().await?;
    let session = remote.create_session(Default::default()).await?;
    let mut devices =
        SelectDevicesOptions::default().set_devices(DeviceType::Keyboard | DeviceType::Pointer);
    // En una sesión compartida, la persistencia y el token van en `SelectDevices` (versión 2).
    if remote.version() >= 2 {
        devices = devices
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(restore_token);
    }
    remote.select_devices(&session, devices).await?.response()?;
    screencast
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(cursor_mode)
                .set_sources(sources)
                .set_multiple(false),
        )
        .await?
        .response()?;
    let selected = remote
        .start(&session, None, Default::default())
        .await?
        .response()?;
    let Some(stream) = selected.streams().first() else {
        return Err(ashpd::Error::Response(ResponseError::Cancelled));
    };
    let node_id = stream.pipe_wire_node_id();
    let token = selected.restore_token().map(str::to_owned);
    let fd = screencast
        .open_pipe_wire_remote(&session, Default::default())
        .await?;
    let eis = remote
        .connect_to_eis(&session, Default::default())
        .await
        .map_err(remote_desktop_error);
    Ok((
        node_id,
        token,
        fd,
        PortalSession::RemoteDesktop(session),
        remote,
        eis,
    ))
}

/// Abre la sesión del portal en un hilo propio y la mantiene viva hasta recibir `stop`. Con `eis`,
/// la sesión es la compartida con `RemoteDesktop` y el socket de EI sale por ahí.
fn spawn_portal(
    restore_token: Option<String>,
    eis: Option<EisSender>,
    sources: Sources,
) -> Result<
    (
        PortalStream,
        tokio::sync::oneshot::Sender<()>,
        JoinHandle<()>,
    ),
    CaptureError,
> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle =
        thread::Builder::new()
            .name("portal".into())
            .spawn(move || {
                let rt = match runtime() {
                    Ok(rt) => rt,
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                        return;
                    }
                };
                rt.block_on(async move {
                let opened = async {
                    tracing::debug!("portal ScreenCast: conectando");
                    let proxy = tokio::time::timeout(PORTAL_TIMEOUT, Screencast::new())
                        .await
                        .map_err(|_| {
                            backend(format!(
                                "el portal ScreenCast no respondió en {} s",
                                PORTAL_TIMEOUT.as_secs()
                            ))
                        })?
                        .map_err(portal_error)?;
                    tracing::debug!(version = proxy.version(), "portal ScreenCast conectado");
                    let cursor_modes =
                        tokio::time::timeout(PORTAL_TIMEOUT, proxy.available_cursor_modes())
                            .await
                            .ok()
                            .and_then(Result::ok)
                            .unwrap_or_default();
                    let cursor_mode = if cursor_modes.contains(CursorMode::Metadata) {
                        CursorMode::Metadata
                    } else {
                        CursorMode::Hidden
                    };
                    let token = restore_token.as_deref();
                    let mut keep = None;
                    let (node_id, restore_token, fd, session) = match &eis {
                        Some(eis) => match tokio::time::timeout(
                            DIALOG_TIMEOUT,
                            open_shared(&proxy, cursor_mode, token, sources),
                        )
                        .await
                        .unwrap_or_else(|_| Err(ashpd::Error::Response(ResponseError::Other)))
                        {
                            Ok((node_id, restore, fd, session, remote, socket)) => {
                                let _ = eis.send(socket.map(|fd| EisSocket { fd }));
                                keep = Some(remote);
                                (node_id, restore, fd, session)
                            }
                            // El usuario canceló el diálogo: no se insiste con otro.
                            Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
                                let _ = eis.send(Err(
                                    "el usuario canceló el diálogo del portal".to_owned()
                                ));
                                return Err(CaptureError::PermissionDenied);
                            }
                            Err(err) => {
                                let reason = remote_desktop_error(err);
                                tracing::warn!(
                                    %reason,
                                    "portal RemoteDesktop sin éxito; se comparte solo la pantalla"
                                );
                                let _ = eis.send(Err(reason));
                                screencast_with_timeout(&proxy, cursor_mode, token, sources).await?
                            }
                        },
                        None => screencast_with_timeout(&proxy, cursor_mode, token, sources).await?,
                    };
                    tracing::debug!(node_id, "portal: sesión de captura abierta");
                    Ok((
                        PortalStream {
                            fd,
                            node_id,
                            restore_token,
                        },
                        (proxy, keep),
                        session,
                    ))
                };
                match opened.await {
                    Ok((stream, _proxies, session)) => {
                        if ready_tx.send(Ok(stream)).is_ok() {
                            // La sesión vive mientras el capturador no pida parar.
                            let _ = stop_rx.await;
                        }
                        session.close().await;
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                    }
                }
            });
            })
            .map_err(|err| backend(format!("no se pudo crear el hilo del portal: {err}")))?;
    match ready_rx.recv() {
        Ok(Ok(stream)) => Ok((stream, stop_tx, handle)),
        Ok(Err(err)) => {
            let _ = handle.join();
            Err(err)
        }
        Err(_) => {
            let _ = handle.join();
            Err(backend("el hilo del portal terminó sin responder"))
        }
    }
}

/// Formato negociado, contador de fotogramas y última forma del cursor, dentro del hilo de
/// PipeWire.
struct StreamState {
    format: VideoInfoRaw,
    /// El formato negociado lleva modificador: los búferes son DMA-BUF.
    dmabuf: bool,
    next_id: u64,
    mailbox: Arc<Mailbox>,
    cursor_shape: Option<Arc<CursorShape>>,
    /// Canal por el que vuelven los búferes DMA-BUF retenidos.
    release: pw::channel::Sender<BufferPtr>,
}

/// Puntero a un `pw_buffer` retenido, para devolverlo por el canal.
struct BufferPtr(*mut pw::sys::pw_buffer);

// SAFETY: el puntero solo se usa en el hilo de PipeWire (que es el que lo recibe por el canal y
// llama a `pw_stream_queue_buffer`); los demás hilos solo lo mueven.
unsafe impl Send for BufferPtr {}
// SAFETY: ver `Send`: nadie lo desreferencia fuera del hilo de PipeWire.
unsafe impl Sync for BufferPtr {}

/// Devuelve un búfer DMA-BUF retenido al compositor al soltarse (en el hilo de PipeWire).
struct BufferRelease {
    buffer: Option<BufferPtr>,
    release: pw::channel::Sender<BufferPtr>,
}

impl Drop for BufferRelease {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            // Si el hilo de PipeWire ya terminó, el flujo y sus búferes ya no existen.
            let _ = self.release.send(buffer);
        }
    }
}

/// Búfer sacado del flujo con `dequeue_raw_buffer`: vuelve a PipeWire al soltarse salvo que se
/// retenga ([`Dequeued::retain`]).
struct Dequeued {
    raw: *mut pw::sys::pw_buffer,
    stream: *mut pw::sys::pw_stream,
}

impl Dequeued {
    /// Saca el siguiente búfer del flujo, si hay.
    fn next(stream: &pw::stream::Stream) -> Option<Self> {
        // SAFETY: se llama desde el callback `process` del propio flujo (hilo de PipeWire); el
        // puntero que devuelve, si no es nulo, es nuestro hasta que se devuelva con
        // `pw_stream_queue_buffer` (lo hace `Drop` o el canal de `BufferRelease`).
        let raw = unsafe { stream.dequeue_raw_buffer() };
        (!raw.is_null()).then(|| Self {
            raw,
            stream: stream.as_raw_ptr(),
        })
    }

    fn spa_buffer(&self) -> *mut spa::sys::spa_buffer {
        // SAFETY: `raw` es un `pw_buffer` válido mientras lo tengamos.
        unsafe { (*self.raw).buffer }
    }

    /// Datos (planos) del búfer.
    fn datas(&self) -> &[Data] {
        let buffer = self.spa_buffer();
        // SAFETY: `spa_buffer` tiene `n_datas` `spa_data` en `datas`, válidos mientras tengamos el
        // búfer; `Data` es `#[repr(transparent)]` sobre `spa_data` (es lo que hace
        // `pipewire::buffer::Buffer::datas_mut`).
        unsafe {
            if buffer.is_null() || (*buffer).datas.is_null() || (*buffer).n_datas == 0 {
                return &[];
            }
            std::slice::from_raw_parts((*buffer).datas as *const Data, (*buffer).n_datas as usize)
        }
    }

    /// Metadatos del cursor, si los hay.
    fn cursor(&self) -> Option<&MetaCursor> {
        let buffer = self.spa_buffer();
        if buffer.is_null() {
            return None;
        }
        // SAFETY: `spa_buffer_find_meta_data` busca en los metadatos del búfer uno de tipo
        // `SPA_META_Cursor` de al menos ese tamaño; `MetaCursor` es `#[repr(transparent)]` sobre
        // `spa_meta_cursor` (lo mismo que hace `Buffer::find_meta`).
        unsafe {
            let meta = spa::sys::spa_buffer_find_meta_data(
                buffer,
                spa::sys::SPA_META_Cursor,
                std::mem::size_of::<spa::sys::spa_meta_cursor>(),
            ) as *const MetaCursor;
            meta.as_ref()
        }
    }

    /// Se queda con el búfer: devuelve el `release` que lo devolverá a PipeWire.
    fn retain(self, release: &pw::channel::Sender<BufferPtr>) -> BufferRelease {
        let buffer = BufferPtr(self.raw);
        std::mem::forget(self);
        BufferRelease {
            buffer: Some(buffer),
            release: release.clone(),
        }
    }
}

impl Drop for Dequeued {
    fn drop(&mut self) {
        // SAFETY: `raw` salió de `dequeue_raw_buffer` de este mismo flujo y no se ha devuelto.
        unsafe { pw::sys::pw_stream_queue_buffer(self.stream, self.raw) };
    }
}

/// Posición de R, G, B y A en un píxel de 4 bytes de los formatos de mapa de bits del cursor
/// (`None` en A: opaco).
fn bitmap_channels(format: VideoFormat) -> Option<([usize; 3], Option<usize>)> {
    Some(if format == VideoFormat::RGBA {
        ([0, 1, 2], Some(3))
    } else if format == VideoFormat::BGRA {
        ([2, 1, 0], Some(3))
    } else if format == VideoFormat::ARGB {
        ([1, 2, 3], Some(0))
    } else if format == VideoFormat::ABGR {
        ([3, 2, 1], Some(0))
    } else if format == VideoFormat::RGBx {
        ([0, 1, 2], None)
    } else if format == VideoFormat::BGRx {
        ([2, 1, 0], None)
    } else {
        return None;
    })
}

/// Convierte el mapa de bits de `SPA_META_Cursor` en una forma RGBA (recortada a
/// [`CURSOR_MAX_SIDE`]). `None` si no trae imagen (PipeWire solo la manda cuando cambia).
fn cursor_shape_from(id: u32, hotspot: (i32, i32), bitmap: &MetaBitmap) -> Option<CursorShape> {
    let (rgb, alpha) = bitmap_channels(bitmap.format())?;
    let size = bitmap.size();
    let stride = bitmap.stride().unsigned_abs() as usize;
    let data = bitmap.bitmap_data()?;
    let width = size.width.min(CURSOR_MAX_SIDE);
    let height = size.height.min(CURSOR_MAX_SIDE);
    if width == 0 || height == 0 || stride < size.width as usize * 4 {
        return None;
    }
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in data.chunks(stride).take(height as usize) {
        for px in row.get(..width as usize * 4)?.as_chunks::<4>().0 {
            rgba.extend_from_slice(&[
                px[rgb[0]],
                px[rgb[1]],
                px[rgb[2]],
                alpha.map_or(255, |a| px[a]),
            ]);
        }
    }
    let clamp = |value: i32, side: u32| u32::try_from(value).unwrap_or(0).min(side - 1);
    let shape = CursorShape {
        shape_id: u64::from(id),
        width,
        height,
        hotspot_x: clamp(hotspot.0, width),
        hotspot_y: clamp(hotspot.1, height),
        rgba,
    };
    shape.is_valid().then_some(shape)
}

/// Si el formato negociado es el HDR de coma flotante (`RGBA_F16`).
fn is_half_float(format: VideoFormat) -> bool {
    format == VideoFormat::RGBA_F16
}

/// Copia un búfer `RGBA_F16` de PipeWire a filas compactas de `width × 8` bytes.
fn copy_half_float(src: &[u8], width: u32, height: u32, src_stride: usize) -> Option<Vec<u8>> {
    let row = width as usize * 8;
    if src_stride < row || src.len() < src_stride * (height as usize - 1) + row {
        return None;
    }
    let mut data = Vec::with_capacity(row * height as usize);
    for line in src.chunks(src_stride).take(height as usize) {
        data.extend_from_slice(line.get(..row)?);
    }
    Some(data)
}

/// Disposición de los canales del formato negociado.
fn channel_order(format: VideoFormat) -> Option<bool> {
    // `Some(false)`: ya es B, G, R, X; `Some(true)`: R, G, B, X y hay que cambiar R por B.
    if format == VideoFormat::BGRx || format == VideoFormat::BGRA {
        Some(false)
    } else if format == VideoFormat::RGBx || format == VideoFormat::RGBA {
        Some(true)
    } else {
        None
    }
}

/// Copia un búfer de PipeWire a BGRA compacto.
fn copy_frame(
    src: &[u8],
    width: u32,
    height: u32,
    src_stride: usize,
    swap_rb: bool,
) -> Option<Vec<u8>> {
    let row = width as usize * 4;
    if src_stride < row || src.len() < src_stride * (height as usize - 1) + row {
        return None;
    }
    let mut data = Vec::with_capacity(row * height as usize);
    for line in src.chunks(src_stride).take(height as usize) {
        let pixels = line.get(..row)?;
        if swap_rb {
            for px in pixels.as_chunks::<4>().0 {
                data.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        } else {
            data.extend_from_slice(pixels);
        }
    }
    Some(data)
}

fn serialize(value: Value) -> Result<Vec<u8>, CaptureError> {
    spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &value)
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|err| backend(format!("parámetros de PipeWire: {err:?}")))
}

/// Formato DRM de un formato de vídeo de 32 bits de SPA.
fn drm_fourcc(format: VideoFormat) -> Option<u32> {
    // Los nombres de SPA dicen el orden en memoria; los de DRM, el de un entero little-endian.
    Some(if format == VideoFormat::BGRx {
        0x3432_5258 // XR24
    } else if format == VideoFormat::BGRA {
        0x3432_5241 // AR24
    } else if format == VideoFormat::RGBx {
        0x3432_4258 // XB24
    } else if format == VideoFormat::RGBA {
        0x3432_4241 // AB24
    } else {
        return None;
    })
}

/// Objeto `EnumFormat` de vídeo sin comprimir con `formats` (el primero, por defecto) y, si se dan,
/// los modificadores DRM obligatorios y sin fijar. `fps` es la cadencia que se propone
/// (`framerate` y `maxFramerate`, hasta 120 si la pantalla los da).
fn video_format(formats: &[VideoFormat], modifiers: Option<&[u64]>, fps: u32) -> Object {
    let fps = fps.clamp(1, 1000);
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    let ids: Vec<Id> = formats.iter().map(|f| Id(f.as_raw())).collect();
    let mut properties = vec![
        Property::new(
            FormatProperties::MediaType.as_raw(),
            Value::Id(Id(MediaType::Video.as_raw())),
        ),
        Property::new(
            FormatProperties::MediaSubtype.as_raw(),
            Value::Id(Id(MediaSubtype::Raw.as_raw())),
        ),
        Property::new(
            FormatProperties::VideoFormat.as_raw(),
            Value::Choice(ChoiceValue::Id(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: ids
                        .first()
                        .copied()
                        .unwrap_or(Id(VideoFormat::BGRx.as_raw())),
                    alternatives: ids.clone(),
                },
            ))),
        ),
    ];
    if let Some(modifiers) = modifiers.filter(|m| !m.is_empty()) {
        let values: Vec<i64> = modifiers.iter().map(|&m| m as i64).collect();
        properties.push(Property {
            key: FormatProperties::VideoModifier.as_raw(),
            // `DONT_FIXATE` (PipeWire ≥ 0.3.33) va por su valor: `libspa` solo nombra la constante
            // con la feature `v0_3_33`.
            flags: PropertyFlags::from_bits_retain(
                PropertyFlags::MANDATORY.bits() | spa::sys::SPA_POD_PROP_FLAG_DONT_FIXATE,
            ),
            value: Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: values[0],
                    alternatives: values,
                },
            ))),
        });
    }
    properties.push(Property::new(
        FormatProperties::VideoSize.as_raw(),
        Value::Choice(ChoiceValue::Rectangle(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: spa::utils::Rectangle {
                    width: 1920,
                    height: 1080,
                },
                min: spa::utils::Rectangle {
                    width: 1,
                    height: 1,
                },
                max: spa::utils::Rectangle {
                    width: 8192,
                    height: 8192,
                },
            },
        ))),
    ));
    properties.push(Property::new(
        FormatProperties::VideoFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: spa::utils::Fraction { num: fps, denom: 1 },
                min: spa::utils::Fraction { num: 0, denom: 1 },
                max: spa::utils::Fraction {
                    num: 1000,
                    denom: 1,
                },
            },
        ))),
    ));
    // Con cadencia variable (`framerate` 0/1, lo que dan mutter y KWin) manda `maxFramerate`.
    properties.push(Property::new(
        FormatProperties::VideoMaxFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: spa::utils::Fraction { num: fps, denom: 1 },
                min: spa::utils::Fraction { num: 1, denom: 1 },
                max: spa::utils::Fraction {
                    num: 1000,
                    denom: 1,
                },
            },
        ))),
    ));
    Object {
        type_: spa::sys::SPA_TYPE_OBJECT_Format,
        id: spa::sys::SPA_PARAM_EnumFormat,
        properties,
    }
}

/// Parámetros `EnumFormat`, en orden de preferencia: con HDR, `RGBA_F16` en memoria compartida;
/// DMA-BUF con `modifiers` (si se piden y no hay HDR) y memoria compartida en los
/// cuatro formatos de 32 bits.
fn format_params(
    modifiers: Option<&[u64]>,
    options: CaptureOptions,
) -> Result<Vec<Vec<u8>>, CaptureError> {
    let fps = options.max_fps;
    let mut params = Vec::with_capacity(3);
    if options.hdr {
        params.push(serialize(Value::Object(video_format(
            &[VideoFormat::RGBA_F16],
            None,
            fps,
        )))?);
    }
    if let Some(modifiers) = modifiers.filter(|m| !m.is_empty() && !options.hdr) {
        params.push(serialize(Value::Object(video_format(
            &[VideoFormat::BGRx, VideoFormat::BGRA],
            Some(modifiers),
            fps,
        )))?);
    }
    params.push(serialize(Value::Object(video_format(
        &[
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ],
        None,
        fps,
    )))?);
    Ok(params)
}

/// `Buffers` (DMA-BUF o memoria compartida, de 4 a 16) y `Meta` (cursor) tras negociar el
/// formato.
fn buffer_params(dmabuf: bool) -> Result<(Vec<u8>, Vec<u8>), CaptureError> {
    let data_type = if dmabuf {
        1 << spa::sys::SPA_DATA_DmaBuf
    } else {
        (1 << spa::sys::SPA_DATA_MemPtr) | (1 << spa::sys::SPA_DATA_MemFd)
    };
    let buffers = Object {
        type_: spa::sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa::sys::SPA_PARAM_Buffers,
        properties: vec![
            Property::new(
                spa::sys::SPA_PARAM_BUFFERS_buffers,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: 6,
                        min: 4,
                        max: 16,
                    },
                ))),
            ),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_dataType, Value::Int(data_type)),
        ],
    };
    let cursor_size = std::mem::size_of::<spa::sys::spa_meta_cursor>()
        + std::mem::size_of::<spa::sys::spa_meta_bitmap>()
        + CURSOR_BITMAP_SIDE * CURSOR_BITMAP_SIDE * 4;
    let meta = Object {
        type_: spa::sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa::sys::SPA_PARAM_Meta,
        properties: vec![
            Property::new(
                spa::sys::SPA_PARAM_META_type,
                Value::Id(Id(spa::sys::SPA_META_Cursor)),
            ),
            Property::new(
                spa::sys::SPA_PARAM_META_size,
                Value::Int(i32::try_from(cursor_size).unwrap_or(i32::MAX)),
            ),
        ],
    };
    Ok((
        serialize(Value::Object(buffers))?,
        serialize(Value::Object(meta))?,
    ))
}

/// Si el formato negociado lleva la propiedad de modificador (búferes DMA-BUF).
fn has_modifier(param: &Pod) -> bool {
    param.as_object().is_ok_and(|object| {
        object
            .find_prop(Id(spa::sys::SPA_FORMAT_VIDEO_modifier))
            .is_some()
    })
}

/// Duplica los descriptores de los planos de un búfer DMA-BUF.
fn dmabuf_planes(datas: &[Data]) -> Option<Vec<DmaBufPlane>> {
    let mut planes = Vec::with_capacity(datas.len());
    for data in datas {
        if data.type_() != DataType::DmaBuf {
            return None;
        }
        let raw = i32::try_from(data.as_raw().fd).ok().filter(|fd| *fd >= 0)?;
        // SAFETY: `raw` es el descriptor DMA-BUF del búfer de PipeWire, abierto mientras el búfer
        // no vuelva al flujo (lo retenemos); `try_clone_to_owned` hace `dup` y el duplicado es
        // nuestro.
        let fd: OwnedFd = unsafe { BorrowedFd::borrow_raw(raw) }
            .try_clone_to_owned()
            .ok()?;
        let chunk = data.chunk();
        planes.push(DmaBufPlane {
            fd,
            offset: chunk.offset(),
            stride: u32::try_from(chunk.stride()).ok()?,
        });
    }
    (!planes.is_empty()).then_some(planes)
}

/// Callback `process`: saca el búfer, lee el cursor y entrega la imagen (DMA-BUF retenido o copia
/// a BGRA).
fn process(stream: &pw::stream::Stream, state: &mut StreamState) {
    let Some(buffer) = Dequeued::next(stream) else {
        return;
    };
    let captured_at = Instant::now();
    let meta = buffer.cursor().filter(|cursor| cursor.is_valid());
    if let Some(meta) = meta
        && let Some(bitmap) = meta.bitmap()
    {
        let hotspot = (meta.hotspot().x, meta.hotspot().y);
        if let Some(shape) = cursor_shape_from(meta.id(), hotspot, bitmap) {
            state.cursor_shape = Some(Arc::new(shape));
        }
    }
    let cursor = meta.map(|cursor| CursorState {
        x: cursor.position().x,
        y: cursor.position().y,
        visible: true,
        shape_id: u64::from(cursor.id()),
    });
    let size = state.format.size();
    let half_float = is_half_float(state.format.format());
    let swap_rb = match channel_order(state.format.format()) {
        Some(swap_rb) => swap_rb,
        None if half_float => false,
        None => return,
    };
    let datas = buffer.datas();
    let Some(data) = datas.first() else {
        return;
    };
    let chunk = data.chunk();
    if chunk.size() == 0
        || chunk.flags().contains(ChunkFlags::CORRUPTED)
        || size.width == 0
        || size.height == 0
    {
        // Búfer solo con metadatos (el cursor se movió y la imagen no cambió) o dañado.
        return;
    }
    let (pixels, cpu_copies) = if state.dmabuf && data.type_() == DataType::DmaBuf {
        let Some(fourcc) = drm_fourcc(state.format.format()) else {
            return;
        };
        let Some(planes) = dmabuf_planes(datas) else {
            return;
        };
        let modifier = state.format.modifier();
        let release: crate::Release = Arc::new(buffer.retain(&state.release));
        (
            FramePixels::DmaBuf(Arc::new(DmaBufFrame {
                planes,
                modifier,
                fourcc,
                width: size.width,
                height: size.height,
                release: Some(release),
            })),
            0,
        )
    } else {
        let (offset, len) = (chunk.offset() as usize, chunk.size() as usize);
        let bytes_per_pixel = if half_float { 8 } else { 4 };
        let stride = match usize::try_from(chunk.stride()) {
            Ok(stride) if stride > 0 => stride,
            _ => size.width as usize * bytes_per_pixel,
        };
        // SAFETY: con `MAP_BUFFERS`, PipeWire tiene mapeada la memoria compartida del plano
        // (`data`, `maxsize` bytes) mientras tengamos el búfer, que no se devuelve hasta el final
        // de este callback; solo se lee.
        let bytes = unsafe {
            let raw = data.as_raw();
            if raw.data.is_null() {
                return;
            }
            std::slice::from_raw_parts(raw.data as *const u8, raw.maxsize as usize)
        };
        let Some(src) = bytes.get(offset..offset.saturating_add(len)) else {
            return;
        };
        if half_float {
            let Some(pixels) = copy_half_float(src, size.width, size.height, stride) else {
                return;
            };
            (
                FramePixels::Rgba16F {
                    data: pixels,
                    stride: size.width * 8,
                },
                1,
            )
        } else {
            let Some(pixels) = copy_frame(src, size.width, size.height, stride, swap_rb) else {
                return;
            };
            (
                FramePixels::Bgra {
                    data: pixels,
                    stride: size.width * 4,
                },
                1,
            )
        }
    };
    let frame_id = state.next_id;
    state.next_id += 1;
    state.mailbox.put(CapturedFrame {
        frame_id,
        width: size.width,
        height: size.height,
        captured_at,
        cursor,
        cursor_shape: state.cursor_shape.clone(),
        pixels,
        cpu_copies,
    });
}

/// Cuerpo del hilo `pipewire`: conecta el `Stream` al nodo y ejecuta el bucle hasta `quit`.
fn run_stream(
    fd: OwnedFd,
    node_id: u32,
    (gpu, options): (GpuFrames, CaptureOptions),
    mailbox: Arc<Mailbox>,
    quit: pw::channel::Receiver<()>,
    ready: std::sync::mpsc::Sender<Result<(), CaptureError>>,
) {
    let setup = || -> Result<(), CaptureError> {
        pw::init();
        let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(backend)?;
        let context = pw::context::ContextRc::new(&main_loop, None).map_err(backend)?;
        let core = context.connect_fd_rc(fd, None).map_err(backend)?;
        let stream = pw::stream::StreamBox::new(
            &core,
            "coroom",
            pw::properties::properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Screen",
            },
        )
        .map_err(backend)?;

        let (release_tx, release_rx) = pw::channel::channel::<BufferPtr>();
        let state = StreamState {
            format: VideoInfoRaw::default(),
            dmabuf: false,
            next_id: 0,
            mailbox: Arc::clone(&mailbox),
            cursor_shape: None,
            release: release_tx,
        };
        let errors = Arc::clone(&mailbox);
        let _listener = stream
            .add_local_listener_with_user_data(state)
            .state_changed(move |_, _, _, new| match new {
                pw::stream::StreamState::Error(message) => {
                    errors.fail(backend(format!("PipeWire: {message}")));
                }
                pw::stream::StreamState::Unconnected => errors.fail(CaptureError::TargetLost),
                _ => {}
            })
            .param_changed(|stream, state, id, param| {
                let Some(param) = param else {
                    return;
                };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                if state.format.parse(param).is_err() {
                    state
                        .mailbox
                        .fail(backend("PipeWire negoció un formato ilegible"));
                    return;
                }
                if channel_order(state.format.format()).is_none()
                    && !is_half_float(state.format.format())
                {
                    state.mailbox.fail(CaptureError::Unsupported(format!(
                        "formato de PipeWire no admitido: {:?}",
                        state.format.format()
                    )));
                    return;
                }
                state.dmabuf = has_modifier(param);
                match buffer_params(state.dmabuf) {
                    Ok((buffers, meta)) => {
                        let (Some(buffers), Some(meta)) =
                            (Pod::from_bytes(&buffers), Pod::from_bytes(&meta))
                        else {
                            return;
                        };
                        if let Err(err) = stream.update_params(&mut [buffers, meta]) {
                            state.mailbox.fail(backend(err));
                        }
                    }
                    Err(err) => state.mailbox.fail(err),
                }
            })
            .process(process)
            .register()
            .map_err(backend)?;

        let modifiers = match &gpu {
            GpuFrames::DmaBuf { modifiers } => Some(modifiers.as_slice()),
            _ => None,
        };
        let formats = format_params(modifiers, options)?;
        let mut pods: Vec<&Pod> = Vec::with_capacity(formats.len());
        for bytes in &formats {
            pods.push(
                Pod::from_bytes(bytes).ok_or_else(|| backend("parámetro de formato no válido"))?,
            );
        }
        stream
            .connect(
                spa::utils::Direction::Input,
                Some(node_id),
                pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
                &mut pods,
            )
            .map_err(backend)?;

        let quit_loop = main_loop.clone();
        let _quit = quit.attach(main_loop.loop_(), move |()| quit_loop.quit());
        let raw_stream = stream.as_raw_ptr();
        // Se suelta antes que `stream` (orden inverso de declaración): no llega ningún búfer a
        // un flujo ya destruido.
        let _release = release_rx.attach(main_loop.loop_(), move |buffer: BufferPtr| {
            // SAFETY: `buffer` es un `pw_buffer` retenido de este flujo (ver `Dequeued::retain`)
            // que vuelve a él en su propio hilo; el flujo sigue vivo mientras este receptor esté
            // enganchado al bucle.
            unsafe { pw::sys::pw_stream_queue_buffer(raw_stream, buffer.0) };
        });
        let _ = ready.send(Ok(()));
        main_loop.run();
        let _ = stream.disconnect();
        Ok(())
    };
    if let Err(err) = setup() {
        mailbox.fail(err.clone());
        let _ = ready.send(Err(err));
    }
}

/// Partes vivas de una captura en marcha.
struct Running {
    quit: pw::channel::Sender<()>,
    stream_thread: JoinHandle<()>,
    portal_stop: tokio::sync::oneshot::Sender<()>,
    portal_thread: JoinHandle<()>,
    mailbox: Arc<Mailbox>,
}

/// Capturador por el portal `ScreenCast` y PipeWire.
#[derive(Default)]
pub struct PipeWireCapturer {
    restore_token: Option<String>,
    running: Option<Running>,
    kind: crate::ShareKind,
    gpu: GpuFrames,
    options: CaptureOptions,
    eis: Option<EisSender>,
}

impl PipeWireCapturer {
    /// Crea un capturador que pedirá permiso con el selector del portal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Crea un capturador que intenta restaurar una selección anterior sin preguntar.
    pub fn with_restore_token(token: impl Into<String>) -> Self {
        Self {
            restore_token: Some(token.into()),
            running: None,
            kind: crate::ShareKind::Screen,
            gpu: GpuFrames::Off,
            options: CaptureOptions::default(),
            eis: None,
        }
    }

    /// La próxima captura abre una sola sesión del portal `RemoteDesktop` + `ScreenCast` y entrega
    /// el socket de libei por lo que devuelve (ver el módulo).
    pub fn share_remote_desktop(&mut self) -> RemoteDesktopShare {
        let (tx, rx) = std::sync::mpsc::channel();
        self.eis = Some(tx);
        RemoteDesktopShare { rx }
    }

    /// Qué ofrece el diálogo del portal: monitores ([`crate::ShareKind::Screen`]) o ventanas
    /// ([`crate::ShareKind::App`]).
    pub fn with_kind(mut self, kind: crate::ShareKind) -> Self {
        self.kind = kind;
        self
    }

    /// Token que devolvió el portal para restaurar esta selección la próxima vez.
    pub fn restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }
}

impl Capturer for PipeWireCapturer {
    fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError> {
        Ok(vec![CaptureTarget::Monitor {
            id: 0,
            name: TARGET_NAME.to_owned(),
        }])
    }

    fn start(&mut self, _target: &CaptureTarget) -> Result<(), CaptureError> {
        self.stop();
        let (portal, portal_stop, portal_thread) = spawn_portal(
            self.restore_token.clone(),
            self.eis.clone(),
            sources(self.kind),
        )?;
        if portal.restore_token.is_some() {
            self.restore_token = portal.restore_token;
        }
        let mailbox = Arc::new(Mailbox::default());
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let stream_mailbox = Arc::clone(&mailbox);
        let (fd, node_id) = (portal.fd, portal.node_id);
        let setup = (self.gpu.clone(), self.options);
        let stream_thread = thread::Builder::new()
            .name("pipewire".into())
            .spawn(move || run_stream(fd, node_id, setup, stream_mailbox, quit_rx, ready_tx));
        let stream_thread = match stream_thread {
            Ok(handle) => handle,
            Err(err) => {
                let _ = portal_stop.send(());
                let _ = portal_thread.join();
                return Err(backend(format!(
                    "no se pudo crear el hilo de PipeWire: {err}"
                )));
            }
        };
        let ready = ready_rx
            .recv()
            .unwrap_or_else(|_| Err(backend("el hilo de PipeWire terminó sin responder")));
        self.running = Some(Running {
            quit,
            stream_thread,
            portal_stop,
            portal_thread,
            mailbox,
        });
        if let Err(err) = ready {
            self.stop();
            return Err(err);
        }
        Ok(())
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        let Some(running) = &self.running else {
            return Err(backend("la captura no está iniciada"));
        };
        running
            .mailbox
            .take(None)?
            .ok_or_else(|| backend("la captura terminó"))
    }

    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        let Some(running) = &self.running else {
            return Err(backend("la captura no está iniciada"));
        };
        running.mailbox.take(Some(timeout))
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = running.quit.send(());
            let _ = running.stream_thread.join();
            let _ = running.portal_stop.send(());
            let _ = running.portal_thread.join();
        }
    }

    /// Con [`GpuFrames::DmaBuf`] y algún modificador, la próxima captura ofrece DMA-BUF antes que
    /// memoria compartida (`COROOM_ZERO_COPY=0` lo impide).
    fn set_gpu_frames(&mut self, frames: GpuFrames) -> GpuFrames {
        self.gpu = match frames {
            GpuFrames::DmaBuf { modifiers } if !modifiers.is_empty() && !zero_copy_disabled() => {
                GpuFrames::DmaBuf { modifiers }
            }
            _ => GpuFrames::Off,
        };
        self.gpu.clone()
    }

    /// Cadencia que se propone al compositor y, con `hdr`, `RGBA_F16` antes que los formatos de 8
    /// bits (si el compositor no lo ofrece, la captura sigue en SDR). SPA aún no lleva la función
    /// de transferencia: el `RGBA_F16` se interpreta como scRGB lineal (1,0 = 80 nits), la
    /// convención de coma flotante de KWin y de Windows.
    fn set_options(&mut self, options: CaptureOptions) -> CaptureOptions {
        self.options = options;
        options
    }
}

/// `COROOM_ZERO_COPY=0` (o `off`, `no`, `false`) desactiva la captura en DMA-BUF.
fn zero_copy_disabled() -> bool {
    std::env::var("COROOM_ZERO_COPY").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "no" | "false"
        )
    })
}

impl Drop for PipeWireCapturer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn capturer_is_send_and_offers_the_portal_target() {
        assert_send::<PipeWireCapturer>();
        let capturer = PipeWireCapturer::with_restore_token("token-de-prueba");
        assert_eq!(capturer.restore_token(), Some("token-de-prueba"));
        assert_eq!(
            capturer.targets().unwrap(),
            vec![CaptureTarget::Monitor {
                id: 0,
                name: TARGET_NAME.into()
            }]
        );
    }

    #[test]
    fn frames_are_copied_to_compact_bgra() {
        // 2×2 en RGBx con 4 bytes de relleno por fila.
        let src = [
            1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, //
            7, 8, 9, 0, 10, 11, 12, 0, 9, 9, 9, 9,
        ];
        let bgra = copy_frame(&src, 2, 2, 12, true).unwrap();
        assert_eq!(
            bgra,
            vec![3, 2, 1, 0, 6, 5, 4, 0, 9, 8, 7, 0, 12, 11, 10, 0]
        );
        let same = copy_frame(&src, 2, 2, 12, false).unwrap();
        assert_eq!(&same[..8], &src[..8]);
        assert!(copy_frame(&src[..10], 2, 2, 12, false).is_none());
        assert_eq!(channel_order(VideoFormat::BGRx), Some(false));
        assert_eq!(channel_order(VideoFormat::RGBA), Some(true));
        assert_eq!(channel_order(VideoFormat::I420), None);
        assert_eq!(
            bitmap_channels(VideoFormat::RGBA),
            Some(([0, 1, 2], Some(3)))
        );
        assert_eq!(bitmap_channels(VideoFormat::BGRx), Some(([2, 1, 0], None)));
        assert_eq!(bitmap_channels(VideoFormat::I420), None);
    }

    #[test]
    fn the_parameters_serialize() {
        let shm = format_params(None, CaptureOptions::default()).unwrap();
        assert_eq!(shm.len(), 1, "solo memoria compartida");
        let hdr = CaptureOptions {
            max_fps: 120,
            hdr: true,
        };
        let params = format_params(Some(&[0]), hdr).unwrap();
        assert_eq!(params.len(), 2, "RGBA_F16 y los de 8 bits, sin DMA-BUF");
        let object = Pod::from_bytes(&params[0]).unwrap().as_object().unwrap();
        assert!(
            object
                .find_prop(Id(spa::sys::SPA_FORMAT_VIDEO_maxFramerate))
                .is_some()
        );
        assert!(is_half_float(VideoFormat::RGBA_F16));
        assert_eq!(
            copy_half_float(&[7u8; 40], 2, 2, 20).map(|d| d.len()),
            Some(32)
        );
        let pod = Pod::from_bytes(&shm[0]).unwrap();
        assert!(!has_modifier(pod));
        for dmabuf in [false, true] {
            let (buffers, meta) = buffer_params(dmabuf).unwrap();
            assert!(Pod::from_bytes(&buffers).is_some());
            assert!(Pod::from_bytes(&meta).is_some());
        }
    }

    #[test]
    fn dmabuf_formats_carry_mandatory_unfixated_modifiers() {
        let modifiers = [0x0100_0000_0000_0002u64, 0];
        let params = format_params(Some(&modifiers), CaptureOptions::default()).unwrap();
        assert_eq!(
            params.len(),
            2,
            "DMA-BUF primero y memoria compartida de respaldo"
        );
        let pod = Pod::from_bytes(&params[0]).unwrap();
        assert!(has_modifier(pod));
        let object = pod.as_object().unwrap();
        let prop = object
            .find_prop(Id(spa::sys::SPA_FORMAT_VIDEO_modifier))
            .expect("propiedad de modificador");
        let flags = prop.flags();
        assert!(flags.contains(spa::pod::PodPropFlags::MANDATORY));
        assert!(flags.contains(spa::pod::PodPropFlags::DONT_FIXATE));
        assert!(!has_modifier(Pod::from_bytes(&params[1]).unwrap()));
        // Sin modificadores no se ofrece DMA-BUF.
        assert_eq!(
            format_params(Some(&[]), CaptureOptions::default())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(drm_fourcc(VideoFormat::BGRx), Some(0x3432_5258));
        assert_eq!(drm_fourcc(VideoFormat::I420), None);
    }

    #[test]
    fn gpu_frames_need_modifiers() {
        let mut capturer = PipeWireCapturer::new();
        assert_eq!(capturer.set_gpu_frames(GpuFrames::D3D11), GpuFrames::Off);
        assert_eq!(
            capturer.set_gpu_frames(GpuFrames::DmaBuf {
                modifiers: Vec::new()
            }),
            GpuFrames::Off
        );
        let asked = GpuFrames::DmaBuf { modifiers: vec![0] };
        let expected = if zero_copy_disabled() {
            GpuFrames::Off
        } else {
            asked.clone()
        };
        assert_eq!(capturer.set_gpu_frames(asked), expected);
    }

    /// Captura real: `COROOM_REAL_CAPTURE=1 cargo test -p coroom-capture -- --ignored`.
    #[test]
    #[ignore = "necesita un escritorio con portal ScreenCast y permiso del usuario"]
    fn captures_real_frames_through_the_portal() {
        if std::env::var_os("COROOM_REAL_CAPTURE").is_none() {
            eprintln!("AVISO: define COROOM_REAL_CAPTURE=1 para la captura real");
            return;
        }
        println!("{}", portal_status().unwrap());
        let mut capturer = PipeWireCapturer::new();
        let target = capturer.targets().unwrap().remove(0);
        capturer.start(&target).unwrap();
        for _ in 0..10 {
            let frame = capturer
                .next_frame_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("un fotograma en 5 s (mueve algo en la pantalla)");
            match &frame.pixels {
                FramePixels::Bgra { data, stride } | FramePixels::Rgba16F { data, stride } => {
                    assert_eq!(data.len(), *stride as usize * frame.height as usize);
                }
                FramePixels::DmaBuf(dmabuf) => println!("DMA-BUF {dmabuf:?}"),
            }
            println!(
                "fotograma {} de {}×{}, cursor {:?}",
                frame.frame_id, frame.width, frame.height, frame.cursor
            );
        }
        capturer.stop();
        println!("token de restauración: {:?}", capturer.restore_token());
    }
}
