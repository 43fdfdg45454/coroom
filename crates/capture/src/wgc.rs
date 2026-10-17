//! Captura en Windows con Windows.Graphics.Capture (crate `windows`).
//!
//! - [`WgcCapturer::targets`]: monitores de `EnumDisplayMonitors` con su nombre de `GetMonitorInfoW`,
//!   el principal primero.
//! - [`Capturer::start`]: `GraphicsCaptureItem` del monitor por `IGraphicsCaptureItemInterop`,
//!   dispositivo D3D11 (`D3D11CreateDevice` con `BGRA_SUPPORT`) envuelto en `IDirect3DDevice`,
//!   `Direct3D11CaptureFramePool::CreateFreeThreaded` en `B8G8R8A8UIntNormalized` con 2 búferes,
//!   sin borde amarillo y sin cursor en la imagen cuando la API lo permite (Windows 10 2004 y 11).
//! - `FrameArrived` (en un hilo del sistema) deja el fotograma en el buzón: gana el último.
//!   - Con [`GpuFrames::D3D11`], sin copia: [`FramePixels::D3D11`] con la textura del
//!     fotograma, el dispositivo y el propio `Direct3D11CaptureFrame`, que no vuelve al pool de WGC
//!     hasta que el codificador la ha copiado (en la GPU) a sus *hw frames*. El pool tiene tres
//!     búferes para que la captura no se quede sin ninguno mientras tanto.
//!   - Si no (respaldo, o `COROOM_ZERO_COPY=0`), copia la textura a una de
//!     *staging*, la mapea y copia el BGRA a CPU.
//!   - Con HDR pedido ([`Capturer::set_options`]), el pool va en `R16G16B16A16Float`
//!     (scRGB lineal: lo que entrega WGC con la pantalla en HDR, y SDR en coma flotante si no) y
//!     el fotograma baja a CPU como [`FramePixels::Rgba16F`] (sin la ruta de texturas).
//! - WGC entrega cada refresco del monitor (`MinUpdateInterval` de 1 ms en Windows 11 24H2; en
//!   versiones anteriores ya entrega por evento a esa frecuencia) y [`FramePacer`] se queda con
//!   los de la cadencia `max_fps` por su `SystemRelativeTime`. Un intervalo mínimo de `1 / max_fps`
//!   redondeaba al múltiplo del refresco: 60 fps con un monitor de 144 Hz daban 48.
//! - El cursor se lee con `GetCursorInfo` y viaja como metadatos; cuando cambia su `HCURSOR`, la
//!   forma se lee con `GetIconInfo` + `GetDIBits` (color con alfa, color con máscara o monocromo
//!   con máscaras AND/XOR) y se convierte a RGBA.
//!
//! ## Ventanas
//!
//! - [`list_windows`]: ventanas de primer nivel de `EnumWindows` que un usuario querría compartir
//!   (visibles, sin ocultar por DWM (`DWMWA_CLOAKED`), sin dueña, que no son de herramientas, con
//!   título y de otro proceso que coroom), ordenadas por título. La clave `app` es el nombre del
//!   ejecutable en minúsculas con extensión (`eldenring.exe`), la misma que usa el audio por
//!   aplicación (`coroom_audio::wasapi::app_key_for_pid`).
//! - [`Capturer::start`] con [`CaptureTarget::Window`]: `CreateForWindow`. Cuando cambia el
//!   tamaño del contenido (la ventana se redimensiona), el propio callback recrea el pool con
//!   `Recreate` al nuevo tamaño; mientras tanto el fotograma se recorta al contenido. Si la ventana
//!   se cierra (evento `Closed` del `GraphicsCaptureItem`, o `IsWindow` falso al esperar
//!   fotogramas) la captura termina con [`CaptureError::TargetLost`].
//! - **Pantalla completa exclusiva**: algunos juegos en pantalla completa exclusiva dan
//!   fotogramas negros (o ninguno) al capturar su ventana. Heurística: mientras no se haya visto
//!   contenido, si han pasado 2 s desde el inicio, la ventana está en primer plano y cubre todo su
//!   monitor, y además
//!   - en la ruta por CPU, todos los fotogramas recibidos eran negros (se mira una rejilla de
//!     8 × 8 píxeles de cada uno), o
//!   - en la ruta sin copia (texturas de GPU, donde leer píxeles obligaría a sincronizar el
//!     contexto con el codificador), no ha llegado ningún fotograma,
//!
//!   se pasa sin avisar a capturar el monitor que contiene la ventana (`MonitorFromWindow`), con
//!   el mismo pool y las mismas opciones, y se deja constancia en el registro de errores. Con el
//!   primer fotograma con contenido (CPU) o el primero que llega (GPU) la heurística se desarma.
//!   Tras el cambio, si la ventana se cierra, también termina con [`CaptureError::TargetLost`].
//!
//! ## Zona capturada y entrada
//!
//! [`captured_area`] da el rectángulo que se está capturando en coordenadas del escritorio virtual
//! (píxeles físicos): el del monitor, o el de la ventana (`DWMWA_EXTENDED_FRAME_BOUNDS`, que es lo
//! que captura WGC), calculado en cada consulta para seguir a la ventana si se mueve. La
//! inyección de ratón absoluto del anfitrión lo usa para llevar las coordenadas del fotograma a la
//! pantalla, y la posición del cursor que viaja con cada fotograma es relativa a él.

// Las llamadas a Win32 y COM de este backend son `unsafe`: cada bloque explica su invariante.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{CloseHandle, HMODULE, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject,
    EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW, GetObjectW, HBITMAP, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow, ReleaseDC,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetCursorInfo,
    GetForegroundWindow, GetIconInfo, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HCURSOR, HICON, ICONINFO,
    IsIconic, IsWindow, IsWindowVisible, MONITORINFOF_PRIMARY, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, IInspectable, Interface, PWSTR, Ref};

use crate::mailbox::Mailbox;
use crate::{
    CURSOR_MAX_SIDE, CaptureError, CaptureOptions, CaptureTarget, CapturedFrame, Capturer,
    CursorShape, CursorState, D3D11Frame, FramePacer, FramePixels, GpuFrames,
};

/// Búferes del pool de captura en memoria de CPU (cada fotograma se copia y se suelta enseguida).
const BUFFERS: i32 = 2;

/// Búferes del pool con texturas para el codificador: uno en el buzón, uno en el canal hacia el
/// codificador y uno libre para la captura.
const GPU_BUFFERS: i32 = 3;

fn backend(context: &str, err: windows::core::Error) -> CaptureError {
    CaptureError::Backend(format!("{context}: {err}"))
}

/// Comprueba que el sistema admite Windows.Graphics.Capture.
pub fn support_status() -> Result<String, CaptureError> {
    match GraphicsCaptureSession::IsSupported() {
        Ok(true) => Ok("Windows.Graphics.Capture admitido".to_owned()),
        Ok(false) => Err(CaptureError::Unsupported(
            "este Windows no admite Windows.Graphics.Capture".into(),
        )),
        Err(err) => Err(backend("Windows.Graphics.Capture", err)),
    }
}

/// Monitor conectado.
#[derive(Debug, Clone)]
struct Monitor {
    handle: HMONITOR,
    name: String,
    primary: bool,
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` es el puntero a `Vec<HMONITOR>` que `list_monitors` pasa a
    // `EnumDisplayMonitors`, vivo y sin otros préstamos durante toda la enumeración, que es
    // síncrona y en el mismo hilo.
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(monitor);
    TRUE
}

fn list_monitors() -> Result<Vec<Monitor>, CaptureError> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: el callback solo usa `data` como `&mut Vec<HMONITOR>` (ver `collect_monitor`) y
    // `handles` vive hasta que `EnumDisplayMonitors` vuelve.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        )
    };
    if !ok.as_bool() {
        return Err(CaptureError::Backend(
            "EnumDisplayMonitors no pudo enumerar los monitores".into(),
        ));
    }
    let mut monitors = Vec::with_capacity(handles.len());
    for (index, handle) in handles.into_iter().enumerate() {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `info` es un `MONITORINFOEXW` con `cbSize` correcto, que empieza por un
        // `MONITORINFO`, como exige `GetMonitorInfoW` para devolver también el nombre.
        let ok = unsafe {
            GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO)
        };
        let (name, primary) = if ok.as_bool() {
            let len = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            let device = String::from_utf16_lossy(&info.szDevice[..len]);
            (
                device.trim_start_matches(r"\\.\").to_owned(),
                info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            )
        } else {
            (format!("Monitor {}", index + 1), false)
        };
        let rect_name = if primary {
            format!("{name} (principal)")
        } else {
            name
        };
        monitors.push(Monitor {
            handle,
            name: rect_name,
            primary,
        });
    }
    monitors.sort_by_key(|monitor| !monitor.primary);
    Ok(monitors)
}

/// Tiempo sin contenido tras el que una ventana en pantalla completa pasa a su monitor.
const FULLSCREEN_GRACE: Duration = Duration::from_secs(2);

/// Cada cuánto se comprueba la ventana (cerrada, pantalla completa) mientras se esperan
/// fotogramas.
const WINDOW_POLL: Duration = Duration::from_millis(250);

/// Rectángulo en coordenadas del escritorio virtual, en píxeles físicos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenArea {
    /// Borde izquierdo.
    pub left: i32,
    /// Borde superior.
    pub top: i32,
    /// Ancho (mayor que 0).
    pub width: i32,
    /// Alto (mayor que 0).
    pub height: i32,
}

impl ScreenArea {
    fn from_rect(rect: RECT) -> Option<Self> {
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        (width > 0 && height > 0).then_some(Self {
            left: rect.left,
            top: rect.top,
            width,
            height,
        })
    }

    /// Si `self` cubre entero a `other`.
    fn covers(&self, other: &Self) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.left + self.width >= other.left + other.width
            && self.top + self.height >= other.top + other.height
    }
}

/// `HWND` guardado como número: el identificador es global del sistema, no un puntero de este
/// proceso, y así los tipos que lo guardan son `Send`.
fn hwnd(id: u64) -> HWND {
    HWND(id as usize as *mut std::ffi::c_void)
}

fn hmonitor(id: u64) -> HMONITOR {
    HMONITOR(id as usize as *mut std::ffi::c_void)
}

/// Qué se captura, para calcular su rectángulo cuando haga falta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `HMONITOR` como número.
    Monitor(u64),
    /// `HWND` como número.
    Window(u64),
}

impl Source {
    fn area(self) -> Option<ScreenArea> {
        match self {
            Self::Monitor(id) => monitor_area(hmonitor(id)),
            Self::Window(id) => window_area(hwnd(id)),
        }
    }
}

fn monitor_area(monitor: HMONITOR) -> Option<ScreenArea> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` es un `MONITORINFO` local con `cbSize` correcto; con un `HMONITOR` que ya no
    // existe la llamada solo falla.
    let ok = unsafe { GetMonitorInfoW(monitor, &mut info) };
    if ok.as_bool() {
        ScreenArea::from_rect(info.rcMonitor)
    } else {
        None
    }
}

/// Rectángulo visible de la ventana (sin la sombra), el mismo que captura WGC.
fn window_area(window: HWND) -> Option<ScreenArea> {
    // SAFETY: `IsWindow` admite cualquier valor; las llamadas siguientes con una ventana que acaba
    // de cerrarse solo fallan.
    if !unsafe { IsWindow(Some(window)) }.as_bool() {
        return None;
    }
    let mut rect = RECT::default();
    // SAFETY: `rect` es un `RECT` local y se pasa su tamaño exacto, como pide
    // `DWMWA_EXTENDED_FRAME_BOUNDS`.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut std::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if dwm.is_err() {
        // SAFETY: `rect` es un `RECT` local donde escribe `GetWindowRect`.
        unsafe { GetWindowRect(window, &mut rect) }.ok()?;
    }
    ScreenArea::from_rect(rect)
}

/// Captura activa del proceso: su generación (para que un `stop` antiguo no borre la de otra) y
/// qué captura.
static ACTIVE: Mutex<Option<(u64, Source)>> = Mutex::new(None);

/// Generaciones de [`ACTIVE`].
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Rectángulo que captura ahora este proceso en coordenadas del escritorio virtual (el monitor, la
/// ventana donde esté ahora, o el monitor de respaldo de una ventana en pantalla completa).
/// `None` si no hay captura o la ventana ya no existe. La inyección de ratón absoluto lo usa para
/// llevar las coordenadas del fotograma a la pantalla.
pub fn captured_area() -> Option<ScreenArea> {
    let source = ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .map(|(_, source)| source)?;
    source.area()
}

unsafe extern "system" fn collect_window(window: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` es el puntero a `Vec<u64>` que `list_windows` pasa a `EnumWindows`, vivo y
    // sin otros préstamos durante toda la enumeración, que es síncrona y en el mismo hilo.
    let list = unsafe { &mut *(data.0 as *mut Vec<u64>) };
    list.push(window.0 as usize as u64);
    TRUE
}

/// Ventanas de primer nivel que se pueden compartir, ordenadas por título (ver el módulo).
pub fn list_windows() -> Result<Vec<CaptureTarget>, CaptureError> {
    let mut handles: Vec<u64> = Vec::new();
    // SAFETY: el callback solo usa `data` como `&mut Vec<u64>` (ver `collect_window`) y `handles`
    // vive hasta que `EnumWindows` vuelve.
    unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut handles as *mut Vec<u64> as isize),
        )
    }
    .map_err(|err| backend("EnumWindows", err))?;
    let own = std::process::id();
    let mut windows: Vec<(String, CaptureTarget)> = handles
        .into_iter()
        .filter_map(|id| shareable_window(id, own))
        .map(|target| {
            let key = match &target {
                CaptureTarget::Window { title, .. } => title.to_lowercase(),
                CaptureTarget::Monitor { name, .. } => name.to_lowercase(),
            };
            (key, target)
        })
        .collect();
    windows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(windows.into_iter().map(|(_, target)| target).collect())
}

/// La ventana `id` como destino si es de las que se comparten (ver el módulo).
fn shareable_window(id: u64, own_pid: u32) -> Option<CaptureTarget> {
    let window = hwnd(id);
    // SAFETY: consultas de solo lectura sobre una ventana de `EnumWindows`; si se cerró entre
    // medias, solo fallan o dan valores vacíos.
    unsafe {
        if !IsWindowVisible(window).as_bool() {
            return None;
        }
        if GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
            return None;
        }
        if GetWindowLongW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
    }
    let mut cloaked: u32 = 0;
    // SAFETY: `DWMWA_CLOAKED` escribe un `DWORD` en `cloaked`, local y del tamaño indicado.
    let cloaked_ok = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    if cloaked_ok.is_ok() && cloaked != 0 {
        return None;
    }
    let mut pid: u32 = 0;
    // SAFETY: `pid` es un `u32` local donde escribe el identificador del proceso.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    if pid == 0 || pid == own_pid {
        return None;
    }
    // SAFETY: solo lee la longitud del título.
    let len = unsafe { GetWindowTextLengthW(window) };
    if len <= 0 {
        return None;
    }
    let mut buffer = vec![0u16; len as usize + 1];
    // SAFETY: `buffer` es un slice local; la API escribe como mucho `buffer.len()` caracteres con
    // el nulo final.
    let copied = unsafe { GetWindowTextW(window, &mut buffer) };
    let copied = usize::try_from(copied).ok()?.min(buffer.len());
    let title = String::from_utf16_lossy(&buffer[..copied])
        .trim()
        .to_owned();
    if title.is_empty() {
        return None;
    }
    Some(CaptureTarget::Window {
        id,
        title,
        app: app_key_for_pid(pid),
    })
}

/// Clave de aplicación del proceso `pid`: nombre del ejecutable en minúsculas con extensión
/// (`eldenring.exe`). Debe coincidir con `coroom_audio::wasapi::app_key_for_pid`.
fn app_key_for_pid(pid: u32) -> Option<String> {
    // SAFETY: abre el proceso solo para consultas limitadas; el `HANDLE` se cierra abajo.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` tiene `size` caracteres de capacidad y `size` es local; la API escribe la
    // ruta y deja en `size` su longitud sin el nulo.
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    // SAFETY: `process` es el `HANDLE` abierto arriba, que no se usa más.
    unsafe {
        let _ = CloseHandle(process);
    }
    result.ok()?;
    let path = String::from_utf16_lossy(&buffer[..(size as usize).min(buffer.len())]);
    let name = path.rsplit(['\\', '/']).next()?.trim();
    (!name.is_empty()).then(|| name.to_lowercase())
}

/// Si algún punto de una rejilla de 8 × 8 tiene color (no es negro). `bytes_per_pixel` es 4
/// (BGRA de 8 bits) u 8 (RGBA en `f16`).
fn has_content(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    bytes_per_pixel: usize,
) -> bool {
    const GRID: u32 = 8;
    for gy in 0..GRID {
        let y = (height - 1) * gy / (GRID - 1).max(1);
        for gx in 0..GRID {
            let x = (width - 1) * gx / (GRID - 1).max(1);
            let at = y as usize * stride + x as usize * bytes_per_pixel;
            let Some(px) = data.get(at..at + bytes_per_pixel) else {
                continue;
            };
            let lit = if bytes_per_pixel == 4 {
                px[..3].iter().any(|&c| c > 8)
            } else {
                // `f16` por canal: magnitud por encima de ~1/128 (exponente sin signo ≥ 0x2000).
                px[..6]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .any(|c| u16::from_le_bytes(*c) & 0x7fff > 0x2000)
            };
            if lit {
                return true;
            }
        }
    }
    false
}

/// Firma de una rejilla de 32 × 32 píxeles (FNV-1a): dos fotogramas con la misma firma se cuentan
/// como repetidos ([`crate::SourceCounters::repeated`]). Basta para el registro: un cambio que no
/// toque ningún punto de la rejilla pasa por repetido.
fn signature(data: &[u8], width: u32, height: u32, stride: usize, bytes_per_pixel: usize) -> u64 {
    const GRID: u32 = 32;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for gy in 0..GRID {
        let y = (height.saturating_sub(1)) * gy / (GRID - 1);
        for gx in 0..GRID {
            let x = (width.saturating_sub(1)) * gx / (GRID - 1);
            let at = y as usize * stride + x as usize * bytes_per_pixel;
            for &byte in data.get(at..at + bytes_per_pixel).unwrap_or(&[]) {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    hash
}

/// Lo que el callback cuenta para la heurística de pantalla completa.
#[derive(Debug, Default)]
struct Watch {
    /// Fotogramas recibidos.
    frames: u64,
    /// Algún fotograma con contenido (solo en la ruta por CPU, donde se puede mirar).
    content: bool,
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device = None;
    let mut context = None;
    // SAFETY: los punteros de salida apuntan a `Option`s locales vivos durante la llamada; sin
    // adaptador explícito se usa el predeterminado con el controlador de hardware.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|err| backend("D3D11CreateDevice", err))?;
    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err(CaptureError::Backend(
            "D3D11CreateDevice no devolvió dispositivo".into(),
        )),
    }
}

/// El dispositivo D3D11 del proceso, uno para todas las capturas. El codificador importa las
/// texturas sin copias solo si son de su mismo dispositivo, y lo toma de la primera captura: si
/// cada captura creara el suyo, cambiar de fuente con la sala abierta rompería la importación
/// («la textura es de otro dispositivo D3D11») y cerraría la sala. Si el dispositivo se perdió
/// (controlador reiniciado), se crea otro.
fn shared_device() -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    static SHARED: Mutex<Option<(ID3D11Device, ID3D11DeviceContext)>> = Mutex::new(None);
    let mut shared = SHARED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((device, context)) = shared.as_ref() {
        // SAFETY: `GetDeviceRemovedReason` solo consulta el estado del dispositivo.
        if unsafe { device.GetDeviceRemovedReason() }.is_ok() {
            return Ok((device.clone(), context.clone()));
        }
        tracing::warn!("el dispositivo D3D11 de la captura se perdió; se crea otro");
    }
    let created = create_device()?;
    *shared = Some(created.clone());
    Ok(created)
}

fn winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice, CaptureError> {
    let dxgi: IDXGIDevice = device.cast().map_err(|err| backend("IDXGIDevice", err))?;
    // SAFETY: `dxgi` es un `IDXGIDevice` válido obtenido del dispositivo D3D11 recién creado.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .map_err(|err| backend("CreateDirect3D11DeviceFromDXGIDevice", err))?;
    inspectable
        .cast()
        .map_err(|err| backend("IDirect3DDevice", err))
}

/// Posición del cursor relativa al monitor capturado y su `HCURSOR`.
fn cursor_state(origin: (i32, i32)) -> Option<(CursorState, HCURSOR)> {
    let mut info = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` es un `CURSORINFO` local con `cbSize` correcto, como pide `GetCursorInfo`.
    unsafe { GetCursorInfo(&mut info) }.ok()?;
    let state = CursorState {
        x: info.ptScreenPos.x - origin.0,
        y: info.ptScreenPos.y - origin.1,
        visible: info.flags.0 & CURSOR_SHOWING.0 != 0,
        shape_id: info.hCursor.0 as usize as u64,
    };
    Some((state, info.hCursor))
}

/// Ancho y alto de un mapa de bits GDI.
fn bitmap_size(bitmap: HBITMAP) -> Option<(u32, u32)> {
    let mut info = BITMAP::default();
    // SAFETY: `bitmap` es un `HBITMAP` vivo (de `GetIconInfo`, aún sin liberar) y `info` es un
    // `BITMAP` local del tamaño que se indica.
    let written = unsafe {
        GetObjectW(
            bitmap.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut std::ffi::c_void),
        )
    };
    let width = u32::try_from(info.bmWidth).ok()?;
    let height = u32::try_from(info.bmHeight).ok()?;
    (written > 0 && width > 0 && height > 0).then_some((width, height))
}

/// Lee `bitmap` como BGRA de 32 bits de arriba abajo (GDI convierte desde su formato).
fn read_bits(hdc: HDC, bitmap: HBITMAP, width: u32, height: u32) -> Option<Vec<u8>> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: i32::try_from(width).ok()?,
            // Negativo: filas de arriba abajo.
            biHeight: -i32::try_from(height).ok()?,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: `hdc` es el DC de pantalla y `bitmap` un mapa de bits vivo que no está seleccionado
    // en ningún DC; `pixels` tiene `width × height × 4` bytes, justo lo que pide una cabecera de
    // 32 bits sin compresión con esas dimensiones, y `info` es local.
    let lines = unsafe {
        GetDIBits(
            hdc,
            bitmap,
            0,
            height,
            Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    (lines == i32::try_from(height).ok()?).then_some(pixels)
}

/// Forma del cursor `cursor` en RGBA, recortada a [`CURSOR_MAX_SIDE`].
fn cursor_shape(cursor: HCURSOR, shape_id: u64) -> Option<CursorShape> {
    let mut icon = ICONINFO::default();
    // SAFETY: `cursor` es el `HCURSOR` que acaba de dar `GetCursorInfo`; `GetIconInfo` escribe en
    // `icon` y crea copias de sus mapas de bits, que se liberan abajo con `DeleteObject`.
    unsafe { GetIconInfo(HICON::from(cursor), &mut icon) }.ok()?;
    // SAFETY: `GetDC(None)` da el DC de la pantalla, que se devuelve con `ReleaseDC` abajo.
    let hdc = unsafe { GetDC(None) };
    let shape = (|| {
        let (mask_w, mask_h) = bitmap_size(icon.hbmMask)?;
        let (width, height, mut bgra) = if icon.hbmColor.is_invalid() {
            // Monocromo: máscara AND arriba y XOR abajo, cada una de `mask_h / 2` filas.
            let height = mask_h / 2;
            let masks = read_bits(hdc, icon.hbmMask, mask_w, mask_h)?;
            let (and, xor) = masks.split_at(mask_w as usize * height as usize * 4);
            let mut out = vec![0u8; and.len()];
            let pairs = and.as_chunks::<4>().0.iter().zip(xor.as_chunks::<4>().0);
            for (px, (a, x)) in out.as_chunks_mut::<4>().0.iter_mut().zip(pairs) {
                let (transparent, invert) = (a[0] != 0, x[0] != 0);
                let value = match (transparent, invert) {
                    (false, false) => [0, 0, 0, 255],
                    (false, true) => [255, 255, 255, 255],
                    (true, false) => [0, 0, 0, 0],
                    // Invertir lo de debajo no se puede reproducir: gris opaco.
                    (true, true) => [128, 128, 128, 255],
                };
                *px = value;
            }
            (mask_w, height, out)
        } else {
            let (width, height) = bitmap_size(icon.hbmColor)?;
            let mut color = read_bits(hdc, icon.hbmColor, width, height)?;
            if color.as_chunks::<4>().0.iter().all(|px| px[3] == 0) {
                // Sin alfa: la máscara AND dice qué píxeles son transparentes.
                let mask = read_bits(hdc, icon.hbmMask, mask_w, mask_h)?;
                for (px, m) in color
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(mask.as_chunks::<4>().0)
                {
                    px[3] = if m[0] != 0 { 0 } else { 255 };
                }
            }
            (width, height, color)
        };
        // BGRA → RGBA y recorte.
        for px in bgra.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        let (w, h) = (width.min(CURSOR_MAX_SIDE), height.min(CURSOR_MAX_SIDE));
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for row in bgra.chunks_exact(width as usize * 4).take(h as usize) {
            rgba.extend_from_slice(&row[..w as usize * 4]);
        }
        Some(CursorShape {
            shape_id,
            width: w,
            height: h,
            hotspot_x: icon.xHotspot.min(w - 1),
            hotspot_y: icon.yHotspot.min(h - 1),
            rgba,
        })
    })();
    // SAFETY: se devuelve el DC de pantalla obtenido arriba y se liberan los mapas de bits que
    // creó `GetIconInfo` (el de color puede ser nulo en los monocromos).
    unsafe {
        ReleaseDC(None, hdc);
        if !icon.hbmColor.is_invalid() {
            let _ = DeleteObject(icon.hbmColor.into());
        }
        if !icon.hbmMask.is_invalid() {
            let _ = DeleteObject(icon.hbmMask.into());
        }
    }
    shape.filter(CursorShape::is_valid)
}

/// Estado del callback `FrameArrived`.
struct FrameCopier {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    /// Contador de fotogramas del capturador (sigue entre capturas, también al pasar al monitor).
    next_id: Arc<AtomicU64>,
    /// Qué se captura: la posición del cursor es relativa a su rectángulo.
    source: Source,
    mailbox: Arc<Mailbox>,
    /// Formato, búferes y tamaño actuales del pool, para recrearlo si cambia el contenido.
    pixel_format: DirectXPixelFormat,
    buffers: i32,
    pool_size: SizeInt32,
    /// Cuenta para la heurística de pantalla completa (solo al capturar una ventana).
    watch: Option<Arc<Mutex<Watch>>>,
    /// Última forma del cursor leída (se relee cuando cambia el `HCURSOR`).
    cursor_shape: Option<Arc<CursorShape>>,
    /// Entregar la textura en vez de copiarla a CPU.
    gpu: bool,
    /// Pool en `R16G16B16A16Float`: 8 bytes por píxel y [`FramePixels::Rgba16F`].
    hdr: bool,
    /// Qué refrescos del monitor se entregan para la cadencia pedida.
    pacer: FramePacer,
    /// Firma del último fotograma copiado a CPU ([`signature`]).
    last_signature: Option<u64>,
}

impl FrameCopier {
    /// Cursor actual y su forma (releída solo si cambió).
    fn cursor(&mut self) -> Option<CursorState> {
        let origin = self
            .source
            .area()
            .map_or((0, 0), |area| (area.left, area.top));
        let (state, handle) = cursor_state(origin)?;
        let stale = self
            .cursor_shape
            .as_ref()
            .is_none_or(|shape| shape.shape_id != state.shape_id);
        if stale && !handle.is_invalid() {
            self.cursor_shape = cursor_shape(handle, state.shape_id).map(Arc::new);
        }
        Some(state)
    }

    fn staging(&mut self, desc: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D, CaptureError> {
        if let Some((texture, width, height)) = &self.staging
            && (*width, *height) == (desc.Width, desc.Height)
        {
            return Ok(texture.clone());
        }
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: desc.SampleDesc,
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: `staging_desc` es una descripción válida de textura de staging del mismo tamaño
        // y formato que la capturada; el puntero de salida apunta a un `Option` local.
        unsafe {
            self.device
                .CreateTexture2D(&staging_desc, None, Some(&mut texture))
        }
        .map_err(|err| backend("CreateTexture2D", err))?;
        let texture = texture
            .ok_or_else(|| CaptureError::Backend("CreateTexture2D no devolvió textura".into()))?;
        self.staging = Some((texture.clone(), desc.Width, desc.Height));
        Ok(texture)
    }

    fn copy(&mut self, pool: &Direct3D11CaptureFramePool) -> Result<(), CaptureError> {
        let frame = pool
            .TryGetNextFrame()
            .map_err(|err| backend("TryGetNextFrame", err))?;
        crate::counters::record_arrived();
        // Sin marca de tiempo (no debería pasar) se entregan todos: el codificador toma el último.
        let arrived = Instant::now();
        let relative = frame
            .SystemRelativeTime()
            .ok()
            .and_then(|time| u64::try_from(time.Duration).ok())
            .map(|ticks| Duration::from_nanos(ticks.saturating_mul(100)));
        if let Some(fps) = self.mailbox.take_max_fps() {
            // Cadencia cambiada en marcha: se eligen otros refrescos de los que llegan.
            self.pacer = FramePacer::new(fps);
        }
        if let Some(relative) = relative
            && !self.pacer.accept(relative)
        {
            crate::counters::record_paced_out();
            let _ = frame.Close();
            return Ok(());
        }
        // La marca `Captured` es la del compositor (`SystemRelativeTime`, el reloj de QPC) si es
        // reciente; si no, la llegada del callback.
        let captured_at = relative
            .and_then(|relative| instant_from_qpc(relative, arrived))
            .unwrap_or(arrived);
        let cursor = self.cursor();
        let content = frame
            .ContentSize()
            .map_err(|err| backend("ContentSize", err))?;
        let access: IDirect3DDxgiInterfaceAccess = frame
            .Surface()
            .and_then(|surface| surface.cast())
            .map_err(|err| backend("IDirect3DDxgiInterfaceAccess", err))?;
        // SAFETY: la superficie de un `Direct3D11CaptureFrame` es una textura D3D11 del dispositivo
        // con el que se creó el pool; `GetInterface` solo hace `QueryInterface`.
        let texture: ID3D11Texture2D =
            unsafe { access.GetInterface() }.map_err(|err| backend("ID3D11Texture2D", err))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` es una descripción local donde `GetDesc` escribe.
        unsafe { texture.GetDesc(&mut desc) };
        let width = desc.Width.min(u32::try_from(content.Width).unwrap_or(0));
        let height = desc.Height.min(u32::try_from(content.Height).unwrap_or(0));
        if content.Width > 0
            && content.Height > 0
            && (content.Width, content.Height) != (self.pool_size.Width, self.pool_size.Height)
        {
            // La ventana cambió de tamaño: los siguientes fotogramas, al tamaño nuevo. El pool es
            // *free-threaded*, así que se puede recrear desde su propio callback; los fotogramas
            // ya entregados conservan sus texturas.
            let device = winrt_device(&self.device)?;
            pool.Recreate(&device, self.pixel_format, self.buffers, content)
                .map_err(|err| backend("Recreate", err))?;
            self.pool_size = content;
        }
        if width == 0 || height == 0 {
            return Ok(());
        }
        // Si la sesión pidió memoria de CPU en marcha (`set_gpu_frames` con `Off`), la textura
        // se copia como siempre: el pool y la sesión de WGC siguen igual.
        if self.gpu && !self.mailbox.shm_only() {
            if let Some(watch) = &self.watch {
                watch.lock().unwrap_or_else(PoisonError::into_inner).frames += 1;
            }
            // Sin copia: la textura y el fotograma, que la retiene, van al codificador.
            let frame_id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let release: crate::Release = Arc::new(frame);
            self.mailbox.put(CapturedFrame {
                frame_id,
                width,
                height,
                captured_at,
                cursor,
                cursor_shape: self.cursor_shape.clone(),
                pixels: FramePixels::D3D11(Arc::new(D3D11Frame {
                    texture,
                    subresource: 0,
                    device: self.device.clone(),
                    release: Some(release),
                })),
                cpu_copies: 0,
            });
            return Ok(());
        }
        let staging = self.staging(&desc)?;

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: ambas texturas son del mismo dispositivo, tamaño y formato; el contexto inmediato
        // solo lo usa este callback, que el pool no ejecuta en paralelo consigo mismo.
        unsafe {
            self.context.CopyResource(&staging, &texture);
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|err| backend("Map", err))?;
        let bytes_per_pixel: u32 = if self.hdr { 8 } else { 4 };
        let row = (width * bytes_per_pixel) as usize;
        let pitch = mapped.RowPitch as usize;
        // Búfer del pool del proceso ([`crate::pool`]): sin reservar memoria por fotograma.
        let mut data = crate::pool::take(row * height as usize);
        let mut copied = false;
        if !mapped.pData.is_null() && pitch >= row {
            // SAFETY: mientras dura el `Map`, `pData` apunta a `RowPitch × Height` bytes legibles de
            // la textura de staging (`desc.Height >= height`).
            let bytes = unsafe {
                std::slice::from_raw_parts(mapped.pData as *const u8, pitch * desc.Height as usize)
            };
            for (dst, line) in data.chunks_exact_mut(row).zip(bytes.chunks(pitch)) {
                dst.copy_from_slice(&line[..row]);
            }
            copied = true;
        }
        // SAFETY: se desmapea el mismo subrecurso que se mapeó; `bytes` ya no se usa.
        unsafe { self.context.Unmap(&staging, 0) };
        drop(frame);
        if !copied {
            crate::pool::give(data);
            return Err(CaptureError::Backend(
                "la textura mapeada no tiene el tamaño esperado".into(),
            ));
        }
        if let Some(watch) = &self.watch {
            let mut watch = watch.lock().unwrap_or_else(PoisonError::into_inner);
            watch.frames += 1;
            if !watch.content && has_content(&data, width, height, row, bytes_per_pixel as usize) {
                watch.content = true;
            }
        }
        // WGC entrega un fotograma también cuando solo se mueve el cursor: si la ventana no se
        // redibuja, todos salen iguales y el registro lo cuenta.
        let signature = signature(&data, width, height, row, bytes_per_pixel as usize);
        if self.last_signature.replace(signature) == Some(signature) {
            crate::counters::record_repeated();
        }
        let frame_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.mailbox.put(CapturedFrame {
            frame_id,
            width,
            height,
            captured_at,
            cursor,
            cursor_shape: self.cursor_shape.clone(),
            pixels: if self.hdr {
                FramePixels::Rgba16F {
                    data,
                    stride: width * 8,
                }
            } else {
                FramePixels::Bgra {
                    data,
                    stride: width * 4,
                }
            },
            // `Map` + copia de la textura de staging.
            cpu_copies: 1,
        });
        Ok(())
    }
}

/// Instante de `Instant` que corresponde a `relative` (`SystemRelativeTime` del fotograma: el
/// reloj de `QueryPerformanceCounter`), calculado con el QPC de ahora: `now - (ahora - relative)`.
/// `None` si la marca es futura o más vieja que un segundo (no es de este reloj o el fotograma se
/// quedó atrás).
fn instant_from_qpc(relative: Duration, now: Instant) -> Option<Instant> {
    let (mut counter, mut frequency) = (0i64, 0i64);
    // SAFETY: `QueryPerformanceCounter` y `QueryPerformanceFrequency` escriben en los enteros
    // locales que se les pasan; no fallan desde Windows XP.
    unsafe {
        QueryPerformanceCounter(&mut counter).ok()?;
        QueryPerformanceFrequency(&mut frequency).ok()?;
    }
    let frequency = u64::try_from(frequency).ok().filter(|f| *f > 0)?;
    let counter = u64::try_from(counter).ok()?;
    let seconds = counter / frequency;
    let nanos = (counter % frequency) * 1_000_000_000 / frequency;
    let qpc_now = Duration::new(seconds, u32::try_from(nanos).ok()?);
    let age = qpc_now.checked_sub(relative)?;
    (age <= Duration::from_secs(1))
        .then(|| now.checked_sub(age))
        .flatten()
}

/// Partes vivas de una captura en marcha.
struct Running {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    item: GraphicsCaptureItem,
    token: i64,
    closed_token: i64,
    mailbox: Arc<Mailbox>,
    /// Generación con la que se anotó en [`ACTIVE`].
    generation: u64,
    /// Ventana capturada (`HWND` como número), también tras pasar a su monitor.
    window: Option<u64>,
    /// Entrega texturas de GPU (no se pueden mirar sus píxeles).
    gpu: bool,
    /// Heurística de pantalla completa armada: cuándo empezó y lo que cuenta el callback.
    fullscreen_watch: Option<(Instant, Arc<Mutex<Watch>>)>,
}

/// Capturador de monitores y ventanas con Windows.Graphics.Capture.
#[derive(Default)]
pub struct WgcCapturer {
    running: Option<Running>,
    gpu: bool,
    options: CaptureOptions,
    next_id: Arc<AtomicU64>,
}

impl WgcCapturer {
    /// Crea un capturador sin iniciar.
    pub fn new() -> Self {
        Self::default()
    }

    fn start_monitor(&mut self, id: u64, window: Option<u64>) -> Result<(), CaptureError> {
        let monitors = list_monitors()?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.handle.0 as usize as u64 == id)
            .or_else(|| monitors.first())
            .ok_or(CaptureError::TargetLost)?
            .clone();
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|err| backend("IGraphicsCaptureItemInterop", err))?;
        // SAFETY: `monitor.handle` viene de `EnumDisplayMonitors` en esta misma llamada.
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor.handle) }
            .map_err(|err| backend("CreateForMonitor", err))?;
        let source = Source::Monitor(monitor.handle.0 as usize as u64);
        self.begin(item, source, window, false)
    }

    fn start_window(&mut self, id: u64) -> Result<(), CaptureError> {
        let window = hwnd(id);
        // SAFETY: `IsWindow` admite cualquier valor.
        if id == 0 || !unsafe { IsWindow(Some(window)) }.as_bool() {
            return Err(CaptureError::TargetLost);
        }
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|err| backend("IGraphicsCaptureItemInterop", err))?;
        // SAFETY: `window` es una ventana que existía justo arriba; si se cerró entre medias,
        // `CreateForWindow` solo falla.
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForWindow(window) }.map_err(|err| {
                // SAFETY: como arriba.
                if unsafe { IsWindow(Some(window)) }.as_bool() {
                    backend("CreateForWindow", err)
                } else {
                    CaptureError::TargetLost
                }
            })?;
        // Para el registro: qué ventana, de qué tamaño y si está minimizada (una ventana
        // minimizada no se redibuja y WGC repite su última imagen).
        // SAFETY: `IsIconic` admite cualquier ventana; la de arriba existía.
        let minimized = unsafe { IsIconic(window) }.as_bool();
        tracing::info!(
            window = %item.DisplayName().unwrap_or_default(),
            size = ?item.Size().map(|size| (size.Width, size.Height)).ok(),
            minimized,
            "WGC: captura de ventana"
        );
        self.begin(item, Source::Window(id), Some(id), true)
    }

    /// Arranca la captura de `item`. Con `watch`, arma la heurística de pantalla completa.
    fn begin(
        &mut self,
        item: GraphicsCaptureItem,
        source: Source,
        window: Option<u64>,
        watch: bool,
    ) -> Result<(), CaptureError> {
        // Cada paso deja huella: un arranque colgado (pasó con un juego en pantalla completa
        // exclusiva) dice en el registro dónde se quedó.
        tracing::info!(?source, "WGC: arranca la captura");
        let (device, context) = shared_device()?;
        let d3d_device = winrt_device(&device)?;
        let size = item
            .Size()
            .map_err(|err| backend("GraphicsCaptureItem", err))?;
        let hdr = self.options.hdr;
        // Las texturas van sin copia solo en BGRA de 8 bits (lo que importan los codificadores).
        let gpu = self.gpu && !hdr;
        let pixel_format = if hdr {
            DirectXPixelFormat::R16G16B16A16Float
        } else {
            DirectXPixelFormat::B8G8R8A8UIntNormalized
        };
        let buffers = if gpu { GPU_BUFFERS } else { BUFFERS };
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d_device,
            pixel_format,
            buffers,
            size,
        )
        .map_err(|err| backend("CreateFreeThreaded", err))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|err| backend("CreateCaptureSession", err))?;
        // Solo existen en versiones recientes: si fallan, se captura con borde o con cursor.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);
        // Windows 11 24H2 limita por defecto la frecuencia de entrega: 1 ms (en unidades de 100 ns)
        // para recibir cada refresco del monitor. La cadencia la elige `FramePacer`. En versiones
        // anteriores no existe y WGC ya entrega a la frecuencia de la pantalla.
        let _ = session.SetMinUpdateInterval(TimeSpan { Duration: 10_000 });

        let mailbox = Arc::new(Mailbox::default());
        let watch = watch.then(|| Arc::new(Mutex::new(Watch::default())));
        let copier = Mutex::new(FrameCopier {
            device,
            context,
            staging: None,
            last_signature: None,
            next_id: Arc::clone(&self.next_id),
            source,
            mailbox: Arc::clone(&mailbox),
            pixel_format,
            buffers,
            pool_size: size,
            watch: watch.clone(),
            cursor_shape: None,
            gpu,
            hdr,
            pacer: FramePacer::new(self.options.max_fps),
        });
        let handler = TypedEventHandler::new(
            move |sender: Ref<Direct3D11CaptureFramePool>, _args: Ref<IInspectable>| {
                let Some(pool) = sender.as_ref() else {
                    return Ok(());
                };
                let mut copier = copier.lock().unwrap_or_else(PoisonError::into_inner);
                if let Err(err) = copier.copy(pool) {
                    copier.mailbox.fail(err);
                }
                Ok(())
            },
        );
        let closed_mailbox = Arc::clone(&mailbox);
        let closed = TypedEventHandler::new(
            move |_item: Ref<GraphicsCaptureItem>, _args: Ref<IInspectable>| {
                closed_mailbox.fail(CaptureError::TargetLost);
                Ok(())
            },
        );
        let closed_token = item
            .Closed(&closed)
            .map_err(|err| backend("GraphicsCaptureItem.Closed", err))?;
        let token = match pool.FrameArrived(&handler) {
            Ok(token) => token,
            Err(err) => {
                let _ = item.RemoveClosed(closed_token);
                return Err(backend("FrameArrived", err));
            }
        };
        if let Err(err) = session.StartCapture() {
            let _ = pool.RemoveFrameArrived(token);
            let _ = item.RemoveClosed(closed_token);
            let _ = session.Close();
            let _ = pool.Close();
            return Err(backend("StartCapture", err));
        }
        tracing::info!(?source, "WGC: captura en marcha");
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        *ACTIVE.lock().unwrap_or_else(PoisonError::into_inner) = Some((generation, source));
        self.running = Some(Running {
            pool,
            session,
            item,
            token,
            closed_token,
            mailbox,
            generation,
            window,
            gpu,
            fullscreen_watch: watch.map(|watch| (Instant::now(), watch)),
        });
        Ok(())
    }

    /// Comprueba la ventana capturada: si se cerró, [`CaptureError::TargetLost`]; si parece en
    /// pantalla completa exclusiva sin imagen, pasa a su monitor (ver el módulo).
    fn check_window(&mut self) -> Result<(), CaptureError> {
        let Some(running) = &mut self.running else {
            return Ok(());
        };
        let Some(id) = running.window else {
            return Ok(());
        };
        let window = hwnd(id);
        // SAFETY: `IsWindow` admite cualquier valor.
        if !unsafe { IsWindow(Some(window)) }.as_bool() {
            return Err(CaptureError::TargetLost);
        }
        let Some((started, watch)) = &running.fullscreen_watch else {
            return Ok(());
        };
        let (frames, content) = {
            let watch = watch.lock().unwrap_or_else(PoisonError::into_inner);
            (watch.frames, watch.content)
        };
        // En la ruta por GPU no se miran píxeles: cualquier fotograma cuenta como contenido.
        if content || (running.gpu && frames > 0) {
            running.fullscreen_watch = None;
            return Ok(());
        }
        if started.elapsed() < FULLSCREEN_GRACE {
            return Ok(());
        }
        // SAFETY: sin precondiciones.
        let foreground = unsafe { GetForegroundWindow() };
        if foreground != window {
            return Ok(());
        }
        // SAFETY: `window` existe (comprobado arriba); con una que acaba de cerrarse, da el
        // monitor más cercano.
        let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
        let (Some(window_area), Some(monitor_area)) = (window_area(window), monitor_area(monitor))
        else {
            return Ok(());
        };
        if !window_area.covers(&monitor_area) {
            return Ok(());
        }
        tracing::warn!(
            "la ventana capturada no da imagen en {} s y ocupa su monitor (¿pantalla completa \
             exclusiva?): se pasa a capturar el monitor",
            FULLSCREEN_GRACE.as_secs()
        );
        self.stop();
        self.start_monitor(monitor.0 as usize as u64, Some(id))
    }
}

impl Capturer for WgcCapturer {
    fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError> {
        Ok(list_monitors()?
            .into_iter()
            .map(|monitor| CaptureTarget::Monitor {
                id: monitor.handle.0 as usize as u64,
                name: monitor.name,
            })
            .collect())
    }

    fn start(&mut self, target: &CaptureTarget) -> Result<(), CaptureError> {
        self.stop();
        match target {
            CaptureTarget::Monitor { id, .. } => self.start_monitor(*id, None),
            CaptureTarget::Window { id, .. } => self.start_window(*id),
        }
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        loop {
            if let Some(frame) = self.next_frame_timeout(WINDOW_POLL)? {
                return Ok(frame);
            }
        }
    }

    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        let deadline = Instant::now() + timeout;
        loop {
            let Some(running) = &self.running else {
                return Err(CaptureError::Backend("la captura no está iniciada".into()));
            };
            let wait = if running.window.is_some() {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(WINDOW_POLL)
            } else {
                deadline.saturating_duration_since(Instant::now())
            };
            if let Some(frame) = running.mailbox.take(Some(wait))? {
                return Ok(Some(frame));
            }
            self.check_window()?;
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            tracing::info!("WGC: se detiene la captura");
            let _ = running.pool.RemoveFrameArrived(running.token);
            let _ = running.item.RemoveClosed(running.closed_token);
            let _ = running.session.Close();
            let _ = running.pool.Close();
            let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
            if active.is_some_and(|(generation, _)| generation == running.generation) {
                *active = None;
            }
        }
    }

    /// Con [`GpuFrames::D3D11`], la próxima captura entrega texturas sin copiarlas a CPU
    /// (`COROOM_ZERO_COPY=0` lo impide). Con [`GpuFrames::Off`], la captura en marcha pasa a
    /// copiar a CPU desde el siguiente fotograma, sin reiniciar la sesión de WGC.
    fn set_gpu_frames(&mut self, frames: GpuFrames) -> GpuFrames {
        let disabled = std::env::var("COROOM_ZERO_COPY").is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "no" | "false"
            )
        });
        self.gpu = frames == GpuFrames::D3D11 && !disabled;
        if let Some(running) = &mut self.running
            && running.gpu
            && !self.gpu
        {
            tracing::info!("captura WGC: pasa a copias en CPU (el codificador no lee texturas)");
            running.mailbox.request_shm();
            running.gpu = false;
        }
        if self.gpu {
            GpuFrames::D3D11
        } else {
            GpuFrames::Off
        }
    }

    /// Intervalo mínimo entre fotogramas y, con `hdr`, captura en `R16G16B16A16Float`. Con la
    /// captura en marcha, la cadencia cambia ya (entre los refrescos que llegan); el HDR, en la
    /// próxima.
    fn set_options(&mut self, options: CaptureOptions) -> CaptureOptions {
        if let Some(running) = &self.running
            && options.max_fps != self.options.max_fps
        {
            running.mailbox.set_max_fps(options.max_fps);
        }
        self.options = options;
        options
    }
}

impl Drop for WgcCapturer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn capturer_is_send() {
        assert_send::<WgcCapturer>();
    }

    /// Captura real: `COROOM_REAL_CAPTURE=1 cargo test -p coroom-capture -- --ignored`.
    #[test]
    #[ignore = "necesita un escritorio de Windows con monitor"]
    fn captures_the_primary_monitor() {
        if std::env::var_os("COROOM_REAL_CAPTURE").is_none() {
            eprintln!("AVISO: define COROOM_REAL_CAPTURE=1 para la captura real");
            return;
        }
        println!("{}", support_status().unwrap());
        let mut capturer = WgcCapturer::new();
        let targets = capturer.targets().unwrap();
        println!("monitores: {targets:?}");
        capturer.start(&targets[0]).unwrap();
        for _ in 0..10 {
            let frame = capturer
                .next_frame_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("un fotograma en 5 s (mueve algo en la pantalla)");
            let FramePixels::Bgra { data, stride } = &frame.pixels else {
                panic!("se esperaba BGRA en CPU");
            };
            assert_eq!(data.len(), *stride as usize * frame.height as usize);
            assert_eq!(frame.cpu_copies, 1);
            println!(
                "fotograma {} de {}×{}, cursor {:?}",
                frame.frame_id, frame.width, frame.height, frame.cursor
            );
        }
        capturer.stop();
    }
}
