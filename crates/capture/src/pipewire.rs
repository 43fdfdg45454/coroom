//! Captura en Linux con el portal `ScreenCast` (`ashpd`) y PipeWire, en DMA-BUF o en memoria
//! compartida.
//!
//! - El portal se habla desde un hilo `portal` con un runtime `tokio` de un solo hilo, que mantiene
//!   viva la sesión (y su conexión D-Bus) mientras dura la captura. Tipos `Monitor | Window`,
//!   cursor como metadatos si el portal lo anuncia (si no, oculto) y `persist_mode` con token de
//!   restauración opcional ([`PipeWireCapturer::with_restore_token`]).
//! - **Entrada del anfitrión** ([`PipeWireCapturer::share_remote_desktop`]): primero la sesión
//!   `ScreenCast` sola, con el selector de monitores y ventanas del escritorio; después, una sesión
//!   `RemoteDesktop` aparte solo de teclado y puntero (`persist_mode` y su propio token en
//!   `SelectDevices`) cuyo `ConnectToEIS` da el socket de libei ([`RemoteDesktopShare`]), junto con
//!   la zona elegida ([`SharedArea`]) para colocar el puntero. No se usa una sola sesión
//!   `RemoteDesktop` con las fuentes encima: el diálogo de KDE para ese caso solo pide permiso y
//!   transmite todos los monitores, sin dejar elegir uno ni una ventana. Si el escritorio no tiene
//!   portal `RemoteDesktop` o el usuario lo rechaza, la captura sigue y la entrega de EI falla con
//!   el motivo.
//! - Con el fd y el id de nodo, un hilo `pipewire` ejecuta `pipewire::main_loop` con un `Stream`
//!   conectado al nodo y metadatos de cursor (`SPA_META_Cursor`, con el mapa de bits de la forma
//!   cuando cambia, que se convierte a RGBA).
//! - **Formatos** (`EnumFormat`), en orden de preferencia: con [`GpuFrames::DmaBuf`],
//!   `video/raw` BGRx o BGRA con la propiedad `SPA_FORMAT_VIDEO_modifier` marcada
//!   `MANDATORY | DONT_FIXATE` y la lista de modificadores que importa el codificador; después,
//!   siempre, BGRx, BGRA, RGBx o RGBA sin modificador, que el compositor da en memoria compartida.
//!   La negociación de DMA-BUF de PipeWire tiene dos vueltas: el compositor contesta
//!   (`param_changed`) con la lista de modificadores que él puede reservar, **sin fijar**, y el
//!   cliente tiene que fijar uno volviendo a anunciar el formato con ese único modificador (y los
//!   formatos de respaldo detrás) con `update_params` ([`negotiate`]); solo con el formato ya
//!   fijado se piden los búferes. Si el compositor no consigue reservar con ese modificador,
//!   vuelve a negociar (otra lista, o memoria compartida) y se repite. Cada estado del flujo,
//!   formato y búfer se registra a nivel `info`, y si en [`FIRST_FRAME_TIMEOUT`] no llega ningún
//!   fotograma, la captura falla diciendo en qué estado y formato se quedó.
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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
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
use pw::spa::pod::deserialize::PodDeserializer;
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, PropertyFlags, Value};
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Id};

use crate::mailbox::Mailbox;
use crate::{
    CURSOR_MAX_SIDE, CaptureError, CaptureOptions, CaptureTarget, CapturedFrame, Capturer,
    CursorShape, CursorState, DmaBufFrame, DmaBufPlane, EisSocket, FramePacer, FramePixels,
    GpuFrames, RemoteDesktopShare, SharedArea,
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

/// Tiempo sin ningún fotograma desde que arranca el flujo tras el que la captura falla con lo que
/// se sabe del flujo (estado, formato, búferes), en vez de esperar en silencio. Solo falla si el
/// flujo no llegó a `Streaming` o no tiene búferes: un flujo en marcha con búferes y sin
/// fotogramas es una fuente que no se repinta (ventana oculta o sin daños en un compositor que
/// solo manda cambios), y eso se avisa una vez y se sigue esperando.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Antigüedad máxima que se acepta en la marca de tiempo del compositor para datar el fotograma
/// con ella; más vieja (o futura), se usa la llegada.
const MAX_PTS_AGE: Duration = Duration::from_secs(1);

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
    /// Token de la sesión `RemoteDesktop` de la entrada.
    input_token: Option<String>,
}

/// Socket de EI que se entrega a la inyección (o por qué no lo hay).
type EisSender = std::sync::mpsc::Sender<Result<EisSocket, String>>;

/// Sesión `RemoteDesktop` de la entrada, viva mientras dura la captura.
struct InputSession {
    _proxy: RemoteDesktop,
    session: Session<RemoteDesktop>,
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

/// Qué eligió el usuario, según lo que cuenta el portal del flujo.
fn shared_area(
    source: Option<SourceType>,
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
) -> SharedArea {
    match (source, position, size) {
        (Some(SourceType::Window), _, Some((width, height))) => SharedArea::Window {
            width: u32::try_from(width).unwrap_or(0),
            height: u32::try_from(height).unwrap_or(0),
        },
        (Some(SourceType::Window), _, None) => SharedArea::Unknown,
        (_, Some((x, y)), _) => SharedArea::Monitor { x, y },
        _ => SharedArea::Unknown,
    }
}

/// Sesión `ScreenCast` abierta.
struct ScreenCastOpen {
    node_id: u32,
    restore_token: Option<String>,
    fd: OwnedFd,
    session: Session<Screencast>,
    area: SharedArea,
    /// Tamaño lógico que dice el portal.
    size: Option<(i32, i32)>,
}

/// Lo que devuelve el portal al arrancar la sesión `ScreenCast`, antes de abrir el socket.
struct Started {
    node_id: u32,
    restore_token: Option<String>,
    area: SharedArea,
    size: Option<(i32, i32)>,
}

/// Sesión `ScreenCast` sola: es la que muestra el selector de monitores y ventanas del escritorio.
/// La sesión se crea fuera del plazo del diálogo ([`screencast_with_timeout`]) para poder
/// cerrarla si el usuario no contesta.
async fn open_screencast(
    proxy: &Screencast,
    session: &Session<Screencast>,
    cursor_mode: CursorMode,
    restore_token: Option<&str>,
    sources: Sources,
) -> Result<Started, CaptureError> {
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
        .select_sources(session, options)
        .await
        .map_err(portal_error)?
        .response()
        .map_err(portal_error)?;
    let streams = proxy
        .start(session, None, Default::default())
        .await
        .map_err(portal_error)?
        .response()
        .map_err(portal_error)?;
    let Some(stream) = streams.streams().first() else {
        return Err(CaptureError::PermissionDenied);
    };
    Ok(Started {
        node_id: stream.pipe_wire_node_id(),
        restore_token: streams.restore_token().map(str::to_owned),
        area: shared_area(stream.source_type(), stream.position(), stream.size()),
        size: stream.size(),
    })
}

/// [`open_screencast`] con [`DIALOG_TIMEOUT`] de límite y el socket de PipeWire. Si el plazo se
/// agota, la sesión del portal se **cierra** antes de devolver el error: así el portal retira el
/// diálogo y la petición pendiente, y una respuesta tardía no llega a un objeto que ya no existe.
async fn screencast_with_timeout(
    proxy: &Screencast,
    cursor_mode: CursorMode,
    restore_token: Option<&str>,
    sources: Sources,
) -> Result<ScreenCastOpen, CaptureError> {
    tracing::debug!("portal ScreenCast: abriendo sesión y diálogo");
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(portal_error)?;
    let started = tokio::time::timeout(
        DIALOG_TIMEOUT,
        open_screencast(proxy, &session, cursor_mode, restore_token, sources),
    )
    .await;
    let started = match started {
        Ok(Ok(started)) => started,
        Ok(Err(err)) => {
            let _ = tokio::time::timeout(PORTAL_TIMEOUT, session.close()).await;
            return Err(err);
        }
        Err(_) => {
            tracing::warn!(
                "el diálogo del portal ScreenCast no se contestó en {} s; se cierra la sesión",
                DIALOG_TIMEOUT.as_secs()
            );
            let _ = tokio::time::timeout(PORTAL_TIMEOUT, session.close()).await;
            return Err(backend(format!(
                "el diálogo del portal ScreenCast no terminó en {} s (¿no se mostró?)",
                DIALOG_TIMEOUT.as_secs()
            )));
        }
    };
    let fd = match tokio::time::timeout(
        PORTAL_TIMEOUT,
        proxy.open_pipe_wire_remote(&session, Default::default()),
    )
    .await
    {
        Ok(Ok(fd)) => fd,
        Ok(Err(err)) => {
            let _ = tokio::time::timeout(PORTAL_TIMEOUT, session.close()).await;
            return Err(portal_error(err));
        }
        Err(_) => {
            let _ = tokio::time::timeout(PORTAL_TIMEOUT, session.close()).await;
            return Err(backend(format!(
                "el portal no abrió el socket de PipeWire en {} s",
                PORTAL_TIMEOUT.as_secs()
            )));
        }
    };
    Ok(ScreenCastOpen {
        node_id: started.node_id,
        restore_token: started.restore_token,
        fd,
        session,
        area: started.area,
        size: started.size,
    })
}

/// Abre el selector del portal para elegir qué compartir antes de abrir la sala y devuelve lo
/// elegido con su token de restauración. La sesión se cierra enseguida: al empezar, la captura
/// repite la elección con el token (el portal no vuelve a preguntar si puede restaurarla).
pub fn pick(kind: crate::ShareKind) -> Result<crate::PickedShare, CaptureError> {
    runtime()?.block_on(async {
        let proxy = tokio::time::timeout(PORTAL_TIMEOUT, Screencast::new())
            .await
            .map_err(|_| {
                backend(format!(
                    "el portal ScreenCast no respondió en {} s",
                    PORTAL_TIMEOUT.as_secs()
                ))
            })?
            .map_err(portal_error)?;
        if proxy.version() < 4 {
            return Err(CaptureError::Unsupported(
                "el portal ScreenCast de este escritorio no recuerda la elección (versión 4 o \
                 posterior): eliges al empezar a transmitir"
                    .into(),
            ));
        }
        let cursor_mode = tokio::time::timeout(PORTAL_TIMEOUT, proxy.available_cursor_modes())
            .await
            .ok()
            .and_then(Result::ok)
            .filter(|modes| modes.contains(CursorMode::Metadata))
            .map_or(CursorMode::Hidden, |_| CursorMode::Metadata);
        let screen = screencast_with_timeout(&proxy, cursor_mode, None, sources(kind)).await?;
        let _ = screen.session.close().await;
        Ok(crate::PickedShare {
            target: picked_target(screen.area, screen.size),
            restore_token: screen.restore_token,
        })
    })
}

/// Destino que se muestra para lo elegido en el portal (Wayland no da nombres: solo el tamaño).
fn picked_target(area: SharedArea, size: Option<(i32, i32)>) -> CaptureTarget {
    let size = size.map(|(w, h)| (w.max(0).unsigned_abs(), h.max(0).unsigned_abs()));
    match (area, size) {
        (SharedArea::Window { width, height }, _) => CaptureTarget::Window {
            id: 0,
            title: format!("{width} × {height}"),
            app: None,
        },
        (_, Some((width, height))) => CaptureTarget::Monitor {
            id: 0,
            name: format!("{width} × {height}"),
        },
        (_, None) => CaptureTarget::Monitor {
            id: 0,
            name: String::new(),
        },
    }
}

/// Sesión `RemoteDesktop` solo de teclado y puntero, después de la de `ScreenCast`. Va aparte
/// porque, con las fuentes de pantalla en la misma sesión, el diálogo de KDE solo pide permiso y
/// transmite todos los monitores: nunca deja elegir uno ni una ventana.
async fn open_input(
    restore_token: Option<&str>,
) -> Result<(InputSession, OwnedFd, Option<String>), ashpd::Error> {
    let remote = RemoteDesktop::new().await?;
    let session = remote.create_session(Default::default()).await?;
    let mut devices =
        SelectDevicesOptions::default().set_devices(DeviceType::Keyboard | DeviceType::Pointer);
    // La persistencia y el token van en `SelectDevices` desde la versión 2.
    if remote.version() >= 2 {
        devices = devices
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(restore_token);
    }
    remote.select_devices(&session, devices).await?.response()?;
    let selected = remote
        .start(&session, None, Default::default())
        .await?
        .response()?;
    let token = selected.restore_token().map(str::to_owned);
    let fd = remote.connect_to_eis(&session, Default::default()).await?;
    Ok((
        InputSession {
            _proxy: remote,
            session,
        },
        fd,
        token,
    ))
}

/// Abre la sesión del portal en un hilo propio y la mantiene viva hasta recibir `stop`. Con `eis`,
/// abre después la sesión `RemoteDesktop` de la entrada y el socket de EI sale por ahí.
fn spawn_portal(
    restore_token: Option<String>,
    input_token: Option<String>,
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
                    let screen = screencast_with_timeout(
                        &proxy,
                        cursor_mode,
                        restore_token.as_deref(),
                        sources,
                    )
                    .await?;
                    tracing::debug!(node_id = screen.node_id, area = ?screen.area, "portal: captura abierta");
                    let mut input = None;
                    let mut new_input_token = None;
                    if let Some(eis) = &eis {
                        let opened = tokio::time::timeout(
                            DIALOG_TIMEOUT,
                            open_input(input_token.as_deref()),
                        )
                        .await
                        .unwrap_or_else(|_| Err(ashpd::Error::Response(ResponseError::Other)));
                        match opened {
                            Ok((session, fd, token)) => {
                                let _ = eis.send(Ok(EisSocket {
                                    fd,
                                    area: screen.area,
                                }));
                                input = Some(session);
                                new_input_token = token;
                            }
                            Err(err) => {
                                let reason = remote_desktop_error(err);
                                tracing::warn!(
                                    %reason,
                                    "portal RemoteDesktop sin éxito; se comparte solo la pantalla"
                                );
                                let _ = eis.send(Err(reason));
                            }
                        }
                    }
                    Ok((
                        PortalStream {
                            fd: screen.fd,
                            node_id: screen.node_id,
                            restore_token: screen.restore_token,
                            input_token: new_input_token,
                        },
                        (proxy, screen.session, input),
                    ))
                };
                match opened.await {
                    Ok((stream, (_proxy, session, input))) => {
                        if ready_tx.send(Ok(stream)).is_ok() {
                            // Las sesiones viven mientras el capturador no pida parar.
                            let _ = stop_rx.await;
                        }
                        if let Some(input) = input {
                            let _ = input.session.close().await;
                        }
                        let _ = session.close().await;
                    }
                    Err(err) => {
                        if let Some(eis) = &eis {
                            let _ = eis.send(Err(format!("no se abrió la captura: {err}")));
                        }
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
    /// Modificadores DRM ofrecidos al compositor (los que importa el codificador).
    offered: Vec<u64>,
    options: CaptureOptions,
    /// Lo que se sabe del flujo, para el error de arranque sin fotogramas.
    diag: Arc<StreamDiag>,
    next_id: u64,
    mailbox: Arc<Mailbox>,
    cursor_shape: Option<Arc<CursorShape>>,
    /// Canal por el que vuelven los búferes DMA-BUF retenidos.
    release: pw::channel::Sender<BufferPtr>,
    /// Qué fotogramas del compositor se entregan para la cadencia pedida.
    pacer: FramePacer,
    /// Marcas de tiempo: las del compositor si las pone en el primer fotograma (`true`); si no,
    /// la llegada desde `epoch`.
    compositor_pts: Option<bool>,
    epoch: Instant,
}

/// Lo que se sabe del flujo de PipeWire desde fuera de su hilo: el último estado, el formato
/// negociado, los búferes añadidos y los fotogramas entregados. Con ello se explica una captura
/// que arranca y no entrega nada ([`FIRST_FRAME_TIMEOUT`]).
#[derive(Default)]
struct StreamDiag {
    state: Mutex<String>,
    format: Mutex<String>,
    buffers: AtomicU32,
    frames: AtomicU64,
    /// Si el flujo llegó alguna vez a `Streaming`.
    streaming: AtomicBool,
}

impl StreamDiag {
    fn set_state(&self, state: &str) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = state.to_owned();
    }

    fn set_format(&self, format: String) {
        *self.format.lock().unwrap_or_else(PoisonError::into_inner) = format;
    }

    /// Resumen para el error de arranque sin fotogramas.
    fn summary(&self) -> String {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let format = self.format.lock().unwrap_or_else(PoisonError::into_inner);
        format!(
            "estado del flujo: {}; formato negociado: {}; búferes añadidos: {}",
            if state.is_empty() {
                "sin cambios (el compositor no conectó el nodo)"
            } else {
                state.as_str()
            },
            if format.is_empty() {
                "ninguno (el compositor no contestó al formato)"
            } else {
                format.as_str()
            },
            self.buffers.load(Ordering::Relaxed)
        )
    }
}

/// Texto de un formato negociado: `BGRx 1920×1080, DMA-BUF (modificador 0x…)` o `memoria
/// compartida`.
fn describe_format(info: &VideoInfoRaw, dmabuf: bool) -> String {
    let size = info.size();
    let memory = if dmabuf {
        format!("DMA-BUF (modificador {:#x})", info.modifier())
    } else {
        "memoria compartida".to_owned()
    };
    format!(
        "{:?} {}×{}, {memory}",
        info.format(),
        size.width,
        size.height
    )
}

/// Nombre del tipo de memoria de un plano de `spa_data`.
fn data_type_name(kind: u32) -> &'static str {
    match kind {
        spa::sys::SPA_DATA_MemPtr => "MemPtr",
        spa::sys::SPA_DATA_MemFd => "MemFd",
        spa::sys::SPA_DATA_DmaBuf => "DmaBuf",
        _ => "otro",
    }
}

/// Puntero a un `pw_buffer` sacado del flujo, para devolverlo por el canal.
struct BufferPtr(*mut pw::sys::pw_buffer);

// SAFETY: el puntero solo se desreferencia en los hilos de PipeWire de este flujo: el de datos lo
// saca y lee en `process`, y el bucle principal, que lo recibe por el canal, llama a
// `pw_stream_queue_buffer`; los demás hilos solo lo mueven.
unsafe impl Send for BufferPtr {}
// SAFETY: ver `Send`: nadie lo desreferencia fuera de los hilos de PipeWire.
unsafe impl Sync for BufferPtr {}

/// Devuelve un búfer DMA-BUF retenido al compositor al soltarse (por el bucle de PipeWire).
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
    /// Por donde vuelve a PipeWire al soltarse (`None` si se retuvo).
    release: Option<pw::channel::Sender<BufferPtr>>,
}

impl Dequeued {
    /// Saca el siguiente búfer del flujo, si hay.
    fn next(stream: &pw::stream::Stream, release: &pw::channel::Sender<BufferPtr>) -> Option<Self> {
        // SAFETY: se llama desde el callback `process` del propio flujo (hilo de datos de
        // PipeWire); el puntero que devuelve, si no es nulo, es nuestro hasta que vuelva al flujo
        // con `pw_stream_queue_buffer` (por el canal `release`, al soltar `Dequeued` o su
        // `BufferRelease`).
        let raw = unsafe { stream.dequeue_raw_buffer() };
        (!raw.is_null()).then(|| Self {
            raw,
            release: Some(release.clone()),
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

    /// Marca de tiempo de presentación del compositor (`spa_meta_header.pts`, en ns del reloj
    /// monótono), si la pone.
    fn pts(&self) -> Option<Duration> {
        let buffer = self.spa_buffer();
        if buffer.is_null() {
            return None;
        }
        // SAFETY: `spa_buffer_find_meta_data` busca en los metadatos del búfer uno de tipo
        // `SPA_META_Header` de al menos ese tamaño; si lo hay, apunta a un `spa_meta_header`
        // válido mientras tengamos el búfer, y aquí solo se lee su `pts`.
        let pts = unsafe {
            let header = spa::sys::spa_buffer_find_meta_data(
                buffer,
                spa::sys::SPA_META_Header,
                std::mem::size_of::<spa::sys::spa_meta_header>(),
            ) as *const spa::sys::spa_meta_header;
            header.as_ref()?.pts
        };
        u64::try_from(pts)
            .ok()
            .filter(|&pts| pts > 0)
            .map(Duration::from_nanos)
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
    fn retain(mut self) -> Option<BufferRelease> {
        // Sin `release`, `Drop` ya no lo devuelve: queda a cargo de `BufferRelease`.
        let release = self.release.take()?;
        Some(BufferRelease {
            buffer: Some(BufferPtr(self.raw)),
            release,
        })
    }
}

impl Drop for Dequeued {
    fn drop(&mut self) {
        // Vuelve por el canal: `pw_stream_queue_buffer` se llama siempre desde el bucle principal
        // (ver `run_stream`). Si el bucle ya no está, el flujo y sus búferes tampoco.
        if let Some(release) = self.release.take() {
            let _ = release.send(BufferPtr(self.raw));
        }
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
    let mut data = crate::pool::take(row * height as usize);
    for (dst, line) in data.chunks_exact_mut(row).zip(src.chunks(src_stride)) {
        dst.copy_from_slice(line.get(..row)?);
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

/// Copia un búfer de PipeWire a BGRA compacto, en un búfer del pool del proceso
/// ([`crate::pool`]): sin reservar memoria nueva por fotograma.
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
    let mut data = crate::pool::take(row * height as usize);
    for (dst, line) in data.chunks_exact_mut(row).zip(src.chunks(src_stride)) {
        dst.copy_from_slice(line.get(..row)?);
        if swap_rb {
            for px in dst.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
    }
    Some(data)
}

/// Instante del reloj de `Instant` que corresponde a `pts` (ns de `CLOCK_MONOTONIC`, la marca del
/// compositor), calculado con la hora monótona de ahora: `now - (ahora_monótono - pts)`. `None`
/// si `pts` es futuro o más viejo que [`MAX_PTS_AGE`] (otro reloj, o un búfer rezagado).
fn instant_from_monotonic(pts: Duration, now: Instant) -> Option<Instant> {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let mono = Duration::new(
        u64::try_from(ts.tv_sec).ok()?,
        u32::try_from(ts.tv_nsec).ok()?,
    );
    let age = mono.checked_sub(pts)?;
    (age <= MAX_PTS_AGE).then(|| now.checked_sub(age)).flatten()
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

/// Modificadores DRM que lleva un `EnumFormat` ([`video_format`]).
#[derive(Debug, Clone, Copy)]
enum ModifierOffer<'a> {
    /// Sin propiedad de modificador: memoria compartida.
    None,
    /// La lista que importa el codificador, obligatoria y sin fijar (primera vuelta).
    Choice(&'a [u64]),
    /// Un modificador ya fijado (segunda vuelta, tras elegir el compositor).
    Fixed(u64),
}

/// Objeto `EnumFormat` de vídeo sin comprimir con `formats` (el primero, por defecto) y los
/// modificadores DRM de `modifiers`. `fps` es la cadencia que se propone (`framerate` y
/// `maxFramerate`, hasta 120 si la pantalla los da).
fn video_format(formats: &[VideoFormat], modifiers: ModifierOffer<'_>, fps: u32) -> Object {
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
    match modifiers {
        ModifierOffer::Choice(modifiers) if !modifiers.is_empty() => {
            let values: Vec<i64> = modifiers.iter().map(|&m| m as i64).collect();
            properties.push(Property {
                key: FormatProperties::VideoModifier.as_raw(),
                // `DONT_FIXATE` (PipeWire ≥ 0.3.33) va por su valor: `libspa` solo nombra la
                // constante con la feature `v0_3_33`.
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
        ModifierOffer::Fixed(modifier) => properties.push(Property {
            key: FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY,
            value: Value::Long(modifier as i64),
        }),
        ModifierOffer::None | ModifierOffer::Choice(_) => {}
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
    // Con cadencia variable (`framerate` 0/1, lo que dan mutter y KWin) manda `maxFramerate`, que
    // el compositor aplica como intervalo mínimo entre fotogramas: pedido igual a `fps`, redondea
    // al múltiplo del refresco (60 con un monitor de 170 Hz daban 56,7). Se pide holgado
    // ([`delivery_cap`]) y la cadencia la elige `FramePacer` por la marca de tiempo.
    properties.push(Property::new(
        FormatProperties::VideoMaxFramerate.as_raw(),
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: spa::utils::Fraction {
                    num: delivery_cap(fps),
                    denom: 1,
                },
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

/// Frecuencia máxima que se le pide al compositor para una cadencia de `fps`: holgada (4 veces y
/// al menos 240), para que no redondee la cadencia al refresco del monitor; los fotogramas que
/// sobran se descartan nada más llegar ([`process`]).
fn delivery_cap(fps: u32) -> u32 {
    fps.saturating_mul(4).clamp(240, 1000)
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
            ModifierOffer::None,
            fps,
        )))?);
    }
    if let Some(modifiers) = modifiers.filter(|m| !m.is_empty() && !options.hdr) {
        params.push(serialize(Value::Object(video_format(
            &[VideoFormat::BGRx, VideoFormat::BGRA],
            ModifierOffer::Choice(modifiers),
            fps,
        )))?);
    }
    params.push(serialize(Value::Object(shm_format(fps)))?);
    Ok(params)
}

/// `EnumFormat` de respaldo en memoria compartida: los cuatro formatos de 32 bits sin modificador.
fn shm_format(fps: u32) -> Object {
    video_format(
        &[
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ],
        ModifierOffer::None,
        fps,
    )
}

/// Segunda vuelta de la negociación de DMA-BUF: `format` con `modifier` ya fijado y, detrás, el
/// respaldo en memoria compartida (si el compositor no puede reservar con ese modificador, cae a
/// él en vez de dejar el flujo sin búferes).
fn fixated_params(
    format: VideoFormat,
    modifier: u64,
    options: CaptureOptions,
) -> Result<Vec<Vec<u8>>, CaptureError> {
    let fps = options.max_fps;
    Ok(vec![
        serialize(Value::Object(video_format(
            &[format],
            ModifierOffer::Fixed(modifier),
            fps,
        )))?,
        serialize(Value::Object(shm_format(fps)))?,
    ])
}

/// Qué toca tras un `param_changed` de formato.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Negotiation {
    /// El compositor contestó con modificadores sin fijar: hay que volver a anunciar el formato
    /// con este, el elegido de entre los que él propone.
    Fixate { modifier: u64 },
    /// Formato fijado: se piden los búferes (DMA-BUF si lleva modificador).
    Ready { dmabuf: bool },
    /// El compositor solo propone (o fijó) modificadores que el codificador no importa, o su
    /// respuesta no se pudo leer: se vuelve a anunciar solo la memoria compartida.
    Shm,
}

/// Lee la propiedad de modificador del formato que contestó el compositor (`format`, el objeto
/// `Format` deserializado) y decide ([`Negotiation`]). Con una lista sin fijar se elige el que el
/// compositor prefiere (su valor por defecto) si está entre los `offered`; si no, el primero de su
/// lista que ofrecimos; si no coincide ninguno (un *tiling* que el codificador no importa ni la CPU
/// puede leer), [`Negotiation::Shm`]: memoria compartida en vez de fotogramas que fallarían uno a
/// uno. Un modificador ya fijado que no ofrecimos también va a memoria compartida.
fn negotiate(format: &Object, offered: &[u64]) -> Negotiation {
    let modifier = format
        .properties
        .iter()
        .find(|property| property.key == spa::sys::SPA_FORMAT_VIDEO_modifier)
        .map(|property| &property.value);
    let pick = |default: i64, alternatives: &[i64]| -> Negotiation {
        let default = default as u64;
        if offered.contains(&default) {
            return Negotiation::Fixate { modifier: default };
        }
        alternatives
            .iter()
            .map(|&m| m as u64)
            .find(|m| offered.contains(m))
            .map_or(Negotiation::Shm, |modifier| Negotiation::Fixate {
                modifier,
            })
    };
    let fixed = |modifier: i64| {
        if offered.contains(&(modifier as u64)) {
            Negotiation::Ready { dmabuf: true }
        } else {
            Negotiation::Shm
        }
    };
    match modifier {
        None => Negotiation::Ready { dmabuf: false },
        Some(Value::Long(modifier)) => fixed(*modifier),
        Some(Value::Choice(ChoiceValue::Long(Choice(_, choice)))) => match choice {
            ChoiceEnum::None(modifier) => fixed(*modifier),
            ChoiceEnum::Enum {
                default,
                alternatives,
            } => pick(*default, alternatives),
            ChoiceEnum::Range { default, .. }
            | ChoiceEnum::Step { default, .. }
            | ChoiceEnum::Flags { default, .. } => pick(*default, &[]),
        },
        // Un modificador de otro tipo no es un formato DMA-BUF que sepamos pedir.
        Some(_) => Negotiation::Shm,
    }
}

/// `Buffers` (DMA-BUF o memoria compartida, de 4 a 16), `Meta` del cursor y `Meta` de la cabecera
/// (la marca de tiempo de presentación del compositor) tras negociar el formato.
/// Parámetros serializados de [`buffer_params`]: búferes, cursor y cabecera.
type BufferParams = (Vec<u8>, Vec<u8>, Vec<u8>);

fn buffer_params(dmabuf: bool) -> Result<BufferParams, CaptureError> {
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
        serialize(Value::Object(header_meta()))?,
    ))
}

/// `Meta` de la cabecera de cada búfer (`spa_meta_header`: su `pts`).
fn header_meta() -> Object {
    Object {
        type_: spa::sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa::sys::SPA_PARAM_Meta,
        properties: vec![
            Property::new(
                spa::sys::SPA_PARAM_META_type,
                Value::Id(Id(spa::sys::SPA_META_Header)),
            ),
            Property::new(
                spa::sys::SPA_PARAM_META_size,
                Value::Int(
                    i32::try_from(std::mem::size_of::<spa::sys::spa_meta_header>())
                        .unwrap_or(i32::MAX),
                ),
            ),
        ],
    }
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
    let Some(buffer) = Dequeued::next(stream, &state.release) else {
        return;
    };
    let arrived = Instant::now();
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
        crate::counters::record_empty();
        return;
    }
    crate::counters::record_arrived();
    // Cadencia pedida: los fotogramas que sobran vuelven al compositor sin copiar nada. Solo
    // cuentan los búferes con imagen: uno solo de cursor ocupaba el hueco de la cadencia y el
    // fotograma del juego que llegaba detrás se descartaba (con el ratón en movimiento, 120 fps
    // pedidos daban unos 30, y ninguno si solo se movía el cursor). Se usan siempre las mismas
    // marcas (las del compositor o la llegada) para no mezclar relojes.
    let pts = buffer.pts();
    let compositor = *state.compositor_pts.get_or_insert(pts.is_some());
    let at = match pts {
        Some(pts) if compositor => pts,
        _ => arrived.saturating_duration_since(state.epoch),
    };
    // La marca `Captured` es la del compositor (cuándo compuso el fotograma) si la pone y es del
    // reloj monótono; si no, la llegada a este callback.
    let captured_at = pts
        .and_then(|pts| instant_from_monotonic(pts, arrived))
        .unwrap_or(arrived);
    if !state.pacer.accept(at) {
        crate::counters::record_paced_out();
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
        let Some(release) = buffer.retain() else {
            return;
        };
        let release: crate::Release = Arc::new(release);
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
    if state.diag.frames.fetch_add(1, Ordering::Relaxed) == 0 {
        tracing::info!(
            width = size.width,
            height = size.height,
            dmabuf = cpu_copies == 0,
            compositor_pts = compositor,
            "PipeWire: primer fotograma"
        );
    }
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
    diag: Arc<StreamDiag>,
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
        let offered: Vec<u64> = match &gpu {
            GpuFrames::DmaBuf { modifiers } => modifiers.clone(),
            _ => Vec::new(),
        };
        let state = StreamState {
            format: VideoInfoRaw::default(),
            dmabuf: false,
            offered,
            options,
            diag,
            next_id: 0,
            mailbox: Arc::clone(&mailbox),
            cursor_shape: None,
            release: release_tx,
            pacer: FramePacer::new(options.max_fps),
            compositor_pts: None,
            epoch: Instant::now(),
        };
        let _listener = stream
            .add_local_listener_with_user_data(state)
            .state_changed(|_, state, old, new| {
                tracing::info!(?old, ?new, "PipeWire: estado del flujo");
                state.diag.set_state(&format!("{new:?}"));
                if matches!(new, pw::stream::StreamState::Streaming) {
                    state.diag.streaming.store(true, Ordering::Relaxed);
                }
                match new {
                    pw::stream::StreamState::Error(message) => {
                        state.mailbox.fail(backend(format!("PipeWire: {message}")));
                    }
                    pw::stream::StreamState::Unconnected => {
                        state.mailbox.fail(CaptureError::TargetLost);
                    }
                    _ => {}
                }
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
                let format = state.format.format();
                if channel_order(format).is_none() && !is_half_float(format) {
                    state.mailbox.fail(CaptureError::Unsupported(format!(
                        "formato de PipeWire no admitido: {format:?}"
                    )));
                    return;
                }
                // El objeto deserializado dice si el modificador viene fijado o como lista; si no
                // se puede leer no se adivina: memoria compartida (sin modificador no hay vuelta
                // de fijación que pueda quedarse a medias).
                let negotiation = match PodDeserializer::deserialize_from::<Value>(param.as_bytes())
                {
                    Ok((_, Value::Object(object))) => negotiate(&object, &state.offered),
                    _ if has_modifier(param) => Negotiation::Shm,
                    _ => Negotiation::Ready { dmabuf: false },
                };
                let update = |stream: &pw::stream::Stream, params: &[Vec<u8>]| {
                    let mut pods: Vec<&Pod> = Vec::with_capacity(params.len());
                    for bytes in params {
                        pods.push(
                            Pod::from_bytes(bytes)
                                .ok_or_else(|| backend("parámetro de PipeWire no válido"))?,
                        );
                    }
                    stream.update_params(&mut pods).map_err(backend)
                };
                match negotiation {
                    Negotiation::Shm => {
                        tracing::info!(
                            ?format,
                            "PipeWire: ningún modificador común con el compositor; se pide memoria \
                             compartida"
                        );
                        state
                            .diag
                            .set_format(format!("{format:?}, pidiendo memoria compartida"));
                        if let Err(err) =
                            serialize(Value::Object(shm_format(state.options.max_fps)))
                                .and_then(|shm| update(stream, &[shm]))
                        {
                            state.mailbox.fail(err);
                        }
                    }
                    Negotiation::Fixate { modifier } => {
                        tracing::info!(
                            ?format,
                            modifier = format_args!("{modifier:#x}"),
                            "PipeWire: el compositor propone modificadores; se fija uno"
                        );
                        state.diag.set_format(format!(
                            "{format:?}, fijando el modificador {modifier:#x}"
                        ));
                        if let Err(err) = fixated_params(format, modifier, state.options)
                            .and_then(|params| update(stream, &params))
                        {
                            state.mailbox.fail(err);
                        }
                    }
                    Negotiation::Ready { dmabuf } => {
                        state.dmabuf = dmabuf;
                        let described = describe_format(&state.format, dmabuf);
                        tracing::info!(format = %described, "PipeWire: formato negociado");
                        state.diag.set_format(described);
                        if let Err(err) =
                            buffer_params(dmabuf).and_then(|(buffers, meta, header)| {
                                update(stream, &[buffers, meta, header])
                            })
                        {
                            state.mailbox.fail(err);
                        }
                    }
                }
            })
            .add_buffer(|_, state, buffer| {
                let count = state.diag.buffers.fetch_add(1, Ordering::Relaxed) + 1;
                // SAFETY: `buffer` es el `pw_buffer` que PipeWire acaba de añadir al flujo,
                // válido durante este callback; solo se lee el tipo de memoria de su primer
                // plano.
                let kind = unsafe {
                    let spa_buffer = (*buffer).buffer;
                    if spa_buffer.is_null() || (*spa_buffer).n_datas == 0 {
                        None
                    } else {
                        Some((*(*spa_buffer).datas).type_)
                    }
                };
                tracing::info!(
                    count,
                    memory = kind.map_or("desconocida", data_type_name),
                    "PipeWire: búfer añadido"
                );
            })
            .remove_buffer(|_, state, _| {
                let before = state.diag.buffers.load(Ordering::Relaxed);
                let left = before.saturating_sub(1);
                state.diag.buffers.store(left, Ordering::Relaxed);
                tracing::info!(left, "PipeWire: búfer retirado");
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
                // `process` en el hilo de datos de PipeWire, que ya tiene prioridad de tiempo
                // real: el fotograma no espera a que se despierte este hilo. Los búferes vuelven
                // al flujo siempre desde el bucle principal (canal `release`): en el hilo de datos
                // solo se sacan, y la cola de devueltos de `pw_stream` es de un solo productor.
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut pods,
            )
            .map_err(backend)?;

        let quit_loop = main_loop.clone();
        let _quit = quit.attach(main_loop.loop_(), move |()| quit_loop.quit());
        let raw_stream = stream.as_raw_ptr();
        // Se suelta antes que `stream` (orden inverso de declaración): no llega ningún búfer a
        // un flujo ya destruido.
        let _release = release_rx.attach(main_loop.loop_(), move |buffer: BufferPtr| {
            // SAFETY: `buffer` es un `pw_buffer` que `process` sacó de este flujo y aún no volvió
            // (ver `Dequeued`); vuelve siempre desde este bucle, el único que lo devuelve, así que
            // la cola de devueltos tiene un solo productor; el flujo sigue vivo mientras este
            // receptor esté enganchado al bucle (se desconecta antes de soltarlo).
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
    diag: Arc<StreamDiag>,
    /// Cuándo arrancó el flujo; hasta el primer fotograma vigila [`FIRST_FRAME_TIMEOUT`].
    started: Instant,
    delivered: bool,
    /// Ya se avisó de que el flujo va pero la fuente no pinta.
    quiet_source_noted: bool,
}

impl Running {
    /// Si pasó [`FIRST_FRAME_TIMEOUT`] sin ningún fotograma: el error que lo explica (dejado
    /// también en el buzón para las llamadas siguientes) si el flujo no arrancó o no tiene
    /// búferes; con el flujo en marcha y búferes, solo un aviso una vez (la fuente no se repinta).
    fn first_frame_watchdog(&mut self) -> Option<CaptureError> {
        if self.delivered || self.started.elapsed() < FIRST_FRAME_TIMEOUT {
            return None;
        }
        let streaming = self.diag.streaming.load(Ordering::Relaxed);
        let buffers = self.diag.buffers.load(Ordering::Relaxed);
        if streaming && buffers > 0 {
            if !self.quiet_source_noted {
                self.quiet_source_noted = true;
                tracing::info!(
                    summary = %self.diag.summary(),
                    "PipeWire: flujo en marcha sin fotogramas en {} s; la fuente no se repinta \
                     (¿ventana oculta o sin cambios?); se sigue esperando",
                    FIRST_FRAME_TIMEOUT.as_secs()
                );
            }
            return None;
        }
        let err = backend(format!(
            "PipeWire no entregó ningún fotograma en {} s ({})",
            FIRST_FRAME_TIMEOUT.as_secs(),
            self.diag.summary()
        ));
        tracing::warn!(%err, "captura sin fotogramas");
        self.mailbox.fail(err.clone());
        Some(err)
    }
}

/// Capturador por el portal `ScreenCast` y PipeWire.
#[derive(Default)]
pub struct PipeWireCapturer {
    restore_token: Option<String>,
    input_token: Option<String>,
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
            input_token: None,
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

    /// Repite la elección de [`pick`] con su token (sin token, el portal pregunta).
    pub fn restoring(mut self, token: Option<String>) -> Self {
        self.restore_token = token;
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
            self.input_token.clone(),
            self.eis.clone(),
            sources(self.kind),
        )?;
        if portal.restore_token.is_some() {
            self.restore_token = portal.restore_token;
        }
        if portal.input_token.is_some() {
            self.input_token = portal.input_token;
        }
        let mailbox = Arc::new(Mailbox::default());
        let diag = Arc::new(StreamDiag::default());
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let stream_mailbox = Arc::clone(&mailbox);
        let stream_diag = Arc::clone(&diag);
        let (fd, node_id) = (portal.fd, portal.node_id);
        let setup = (self.gpu.clone(), self.options);
        tracing::info!(
            node_id,
            dmabuf_modifiers = match &self.gpu {
                GpuFrames::DmaBuf { modifiers } => modifiers.len(),
                _ => 0,
            },
            max_fps = self.options.max_fps,
            hdr = self.options.hdr,
            "PipeWire: conectando al nodo del portal"
        );
        let stream_thread = thread::Builder::new()
            .name("pipewire".into())
            .spawn(move || {
                run_stream(
                    fd,
                    node_id,
                    setup,
                    stream_mailbox,
                    stream_diag,
                    quit_rx,
                    ready_tx,
                )
            });
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
            diag,
            started: Instant::now(),
            delivered: false,
            quiet_source_noted: false,
        });
        if let Err(err) = ready {
            self.stop();
            return Err(err);
        }
        Ok(())
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        let Some(running) = &mut self.running else {
            return Err(backend("la captura no está iniciada"));
        };
        let frame = running
            .mailbox
            .take(None)?
            .ok_or_else(|| backend("la captura terminó"))?;
        running.delivered = true;
        Ok(frame)
    }

    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        let Some(running) = &mut self.running else {
            return Err(backend("la captura no está iniciada"));
        };
        let frame = running.mailbox.take(Some(timeout))?;
        if frame.is_some() {
            running.delivered = true;
        } else if let Some(err) = running.first_frame_watchdog() {
            return Err(err);
        }
        Ok(frame)
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
    fn shared_area_follows_what_the_portal_reports() {
        assert_eq!(
            shared_area(
                Some(SourceType::Monitor),
                Some((2560, 0)),
                Some((1920, 1080))
            ),
            SharedArea::Monitor { x: 2560, y: 0 }
        );
        assert_eq!(
            shared_area(Some(SourceType::Window), Some((10, 20)), Some((800, 600))),
            SharedArea::Window {
                width: 800,
                height: 600
            },
            "la posición de una ventana no sirve para colocar el puntero"
        );
        assert_eq!(
            shared_area(None, Some((-1920, 0)), None),
            SharedArea::Monitor { x: -1920, y: 0 }
        );
        assert_eq!(shared_area(None, None, None), SharedArea::Unknown);
        assert_eq!(
            shared_area(Some(SourceType::Window), None, None),
            SharedArea::Unknown
        );
    }

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
            let (buffers, meta, header) = buffer_params(dmabuf).unwrap();
            assert!(Pod::from_bytes(&buffers).is_some());
            assert!(Pod::from_bytes(&meta).is_some());
            assert!(Pod::from_bytes(&header).is_some());
        }
    }

    /// El compositor no debe redondear la cadencia: se le pide holgada para 30, 60 y 120.
    #[test]
    fn the_compositor_is_asked_for_more_than_the_cadence() {
        assert_eq!(delivery_cap(30), 240);
        assert_eq!(delivery_cap(60), 240);
        assert_eq!(delivery_cap(120), 480);
        assert_eq!(delivery_cap(500), 1000);
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

    /// Deserializa el primer `EnumFormat` de `params` como objeto.
    fn first_object(params: &[Vec<u8>]) -> Object {
        match PodDeserializer::deserialize_from::<Value>(&params[0]) {
            Ok((_, Value::Object(object))) => object,
            other => panic!("no es un objeto: {other:?}"),
        }
    }

    /// La negociación de DMA-BUF en dos vueltas: con la lista sin fijar del compositor se fija el
    /// que él prefiere si lo ofrecimos (o el primero común); con el modificador fijado o sin
    /// modificador se piden los búferes.
    #[test]
    fn dmabuf_negotiation_fixates_a_common_modifier_and_then_asks_for_buffers() {
        let offered = [0x0100_0000_0000_0002u64, 0x0100_0000_0000_0005, 0];
        // Lo que contestaría el compositor: su lista (sin fijar), con el preferido primero.
        let answer = |modifiers: &[u64]| {
            serialize(Value::Object(video_format(
                &[VideoFormat::BGRx],
                ModifierOffer::Choice(modifiers),
                60,
            )))
            .unwrap()
        };
        assert_eq!(
            negotiate(
                &first_object(&[answer(&[0x0100_0000_0000_0005, 0])]),
                &offered
            ),
            Negotiation::Fixate {
                modifier: 0x0100_0000_0000_0005
            },
            "el preferido del compositor, si lo ofrecimos"
        );
        assert_eq!(
            negotiate(
                &first_object(&[answer(&[0x77, 0, 0x0100_0000_0000_0002])]),
                &offered
            ),
            Negotiation::Fixate { modifier: 0 },
            "si no, el primero de su lista que ofrecimos"
        );
        assert_eq!(
            negotiate(&first_object(&[answer(&[0x77, 0x78])]), &offered),
            Negotiation::Shm,
            "sin ninguno común, memoria compartida en vez de DMA-BUF que nadie importa"
        );
        // Un modificador fijado por el compositor que no ofrecimos tampoco sirve.
        let foreign = fixated_params(VideoFormat::BGRx, 0x77, CaptureOptions::default()).unwrap();
        assert_eq!(
            negotiate(&first_object(&foreign), &offered),
            Negotiation::Shm
        );
        assert_eq!(
            negotiate(&first_object(&foreign), &[]),
            Negotiation::Shm,
            "sin nada ofrecido, nada de DMA-BUF"
        );
        // Segunda vuelta: el formato fijado y el respaldo en memoria compartida.
        let fixated = fixated_params(
            VideoFormat::BGRx,
            0x0100_0000_0000_0005,
            CaptureOptions::default(),
        )
        .unwrap();
        assert_eq!(fixated.len(), 2);
        let pod = Pod::from_bytes(&fixated[0]).unwrap();
        let prop = pod
            .as_object()
            .unwrap()
            .find_prop(Id(spa::sys::SPA_FORMAT_VIDEO_modifier))
            .expect("modificador");
        assert!(prop.flags().contains(spa::pod::PodPropFlags::MANDATORY));
        assert!(
            !prop.flags().contains(spa::pod::PodPropFlags::DONT_FIXATE),
            "fijado: ya no es una lista"
        );
        assert_eq!(prop.value().get_long(), Ok(0x0100_0000_0000_0005));
        assert_eq!(
            negotiate(&first_object(&fixated), &offered),
            Negotiation::Ready { dmabuf: true }
        );
        assert_eq!(
            negotiate(&first_object(&[fixated[1].clone()]), &offered),
            Negotiation::Ready { dmabuf: false },
            "memoria compartida: sin modificador"
        );
        assert!(!has_modifier(Pod::from_bytes(&fixated[1]).unwrap()));
    }

    #[test]
    fn stream_diagnostics_explain_an_empty_start() {
        let diag = StreamDiag::default();
        assert!(diag.summary().contains("no conectó el nodo"));
        assert!(!diag.streaming.load(Ordering::Relaxed));
        diag.set_state("Paused");
        diag.set_format("BGRx 1920×1080, memoria compartida".to_owned());
        diag.buffers.store(6, Ordering::Relaxed);
        let summary = diag.summary();
        assert!(summary.contains("Paused") && summary.contains("1920×1080"));
        assert!(summary.contains("búferes añadidos: 6"));
        assert_eq!(data_type_name(spa::sys::SPA_DATA_DmaBuf), "DmaBuf");
        // Una marca del compositor de hace 2 ms data el fotograma 2 ms antes de la llegada.
        let now = Instant::now();
        let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let mono = Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32);
        let dated = instant_from_monotonic(mono - Duration::from_millis(2), now).unwrap();
        assert!(now.duration_since(dated) >= Duration::from_millis(2));
        assert!(now.duration_since(dated) < Duration::from_millis(500));
        assert!(instant_from_monotonic(mono + Duration::from_secs(5), now).is_none());
        assert!(instant_from_monotonic(mono - Duration::from_secs(5), now).is_none());
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
