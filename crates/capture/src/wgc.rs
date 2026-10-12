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
//! - Con `max_fps`, `MinUpdateInterval` de la sesión a `1 / max_fps` (Windows 11 24H2;
//!   en versiones anteriores WGC ya entrega por evento a la frecuencia de la pantalla).
//! - El cursor se lee con `GetCursorInfo` y viaja como metadatos; cuando cambia su `HCURSOR`, la
//!   forma se lee con `GetIconInfo` + `GetDIBits` (color con alfa, color con máscara o monocromo
//!   con máscaras AND/XOR) y se convierte a RGBA.

// Las llamadas a Win32 y COM de este backend son `unsafe`: cada bloque explica su invariante.
#![allow(unsafe_code)]

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject,
    EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW, GetObjectW, HBITMAP, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, ReleaseDC,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, HCURSOR, HICON, ICONINFO,
    MONITORINFOF_PRIMARY,
};
use windows::core::{BOOL, IInspectable, Interface, Ref};

use crate::mailbox::Mailbox;
use crate::{
    CURSOR_MAX_SIDE, CaptureError, CaptureOptions, CaptureTarget, CapturedFrame, Capturer,
    CursorShape, CursorState, D3D11Frame, FramePixels, GpuFrames,
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
    origin: (i32, i32),
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
        let (name, primary, origin) = if ok.as_bool() {
            let len = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            let device = String::from_utf16_lossy(&info.szDevice[..len]);
            let rect = info.monitorInfo.rcMonitor;
            (
                device.trim_start_matches(r"\\.\").to_owned(),
                info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                (rect.left, rect.top),
            )
        } else {
            (format!("Monitor {}", index + 1), false, (0, 0))
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
            origin,
        });
    }
    monitors.sort_by_key(|monitor| !monitor.primary);
    Ok(monitors)
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
    next_id: u64,
    origin: (i32, i32),
    mailbox: Arc<Mailbox>,
    /// Última forma del cursor leída (se relee cuando cambia el `HCURSOR`).
    cursor_shape: Option<Arc<CursorShape>>,
    /// Entregar la textura en vez de copiarla a CPU.
    gpu: bool,
    /// Pool en `R16G16B16A16Float`: 8 bytes por píxel y [`FramePixels::Rgba16F`].
    hdr: bool,
}

impl FrameCopier {
    /// Cursor actual y su forma (releída solo si cambió).
    fn cursor(&mut self) -> Option<CursorState> {
        let (state, handle) = cursor_state(self.origin)?;
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
        let captured_at = Instant::now();
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
        if width == 0 || height == 0 {
            return Ok(());
        }
        if self.gpu {
            // Sin copia: la textura y el fotograma, que la retiene, van al codificador.
            let frame_id = self.next_id;
            self.next_id += 1;
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
        let mut data = Vec::with_capacity(row * height as usize);
        if !mapped.pData.is_null() && pitch >= row {
            // SAFETY: mientras dura el `Map`, `pData` apunta a `RowPitch × Height` bytes legibles de
            // la textura de staging (`desc.Height >= height`).
            let bytes = unsafe {
                std::slice::from_raw_parts(mapped.pData as *const u8, pitch * desc.Height as usize)
            };
            for line in bytes.chunks(pitch).take(height as usize) {
                data.extend_from_slice(&line[..row]);
            }
        }
        // SAFETY: se desmapea el mismo subrecurso que se mapeó; `bytes` ya no se usa.
        unsafe { self.context.Unmap(&staging, 0) };
        drop(frame);
        if data.len() != row * height as usize {
            return Err(CaptureError::Backend(
                "la textura mapeada no tiene el tamaño esperado".into(),
            ));
        }
        let frame_id = self.next_id;
        self.next_id += 1;
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

/// Partes vivas de una captura en marcha.
struct Running {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
    mailbox: Arc<Mailbox>,
}

/// Capturador de monitores con Windows.Graphics.Capture.
#[derive(Default)]
pub struct WgcCapturer {
    running: Option<Running>,
    gpu: bool,
    options: CaptureOptions,
}

impl WgcCapturer {
    /// Crea un capturador sin iniciar.
    pub fn new() -> Self {
        Self::default()
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
        let CaptureTarget::Monitor { id, .. } = target else {
            return Err(CaptureError::Unsupported(
                "por ahora solo se capturan monitores en Windows".into(),
            ));
        };
        let monitors = list_monitors()?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.handle.0 as usize as u64 == *id)
            .or_else(|| monitors.first())
            .ok_or(CaptureError::TargetLost)?
            .clone();

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|err| backend("IGraphicsCaptureItemInterop", err))?;
        // SAFETY: `monitor.handle` viene de `EnumDisplayMonitors` en esta misma llamada.
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor.handle) }
            .map_err(|err| backend("CreateForMonitor", err))?;
        let (device, context) = create_device()?;
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
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d_device,
            pixel_format,
            if gpu { GPU_BUFFERS } else { BUFFERS },
            size,
        )
        .map_err(|err| backend("CreateFreeThreaded", err))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|err| backend("CreateCaptureSession", err))?;
        // Solo existen en versiones recientes: si fallan, se captura con borde o con cursor.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);
        // Windows 11 24H2: no más de `max_fps` fotogramas por segundo (en unidades de 100 ns). En
        // versiones anteriores no existe y WGC entrega a la frecuencia de la pantalla.
        let fps = i64::from(self.options.max_fps.max(1));
        let _ = session.SetMinUpdateInterval(TimeSpan {
            Duration: 10_000_000 / fps,
        });

        let mailbox = Arc::new(Mailbox::default());
        let copier = Mutex::new(FrameCopier {
            device,
            context,
            staging: None,
            next_id: 0,
            origin: monitor.origin,
            mailbox: Arc::clone(&mailbox),
            cursor_shape: None,
            gpu,
            hdr,
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
        let token = pool
            .FrameArrived(&handler)
            .map_err(|err| backend("FrameArrived", err))?;
        session
            .StartCapture()
            .map_err(|err| backend("StartCapture", err))?;
        self.running = Some(Running {
            pool,
            session,
            token,
            mailbox,
        });
        Ok(())
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        let Some(running) = &self.running else {
            return Err(CaptureError::Backend("la captura no está iniciada".into()));
        };
        running
            .mailbox
            .take(None)?
            .ok_or_else(|| CaptureError::Backend("la captura terminó".into()))
    }

    fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        let Some(running) = &self.running else {
            return Err(CaptureError::Backend("la captura no está iniciada".into()));
        };
        running.mailbox.take(Some(timeout))
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = running.pool.RemoveFrameArrived(running.token);
            let _ = running.session.Close();
            let _ = running.pool.Close();
        }
    }

    /// Con [`GpuFrames::D3D11`], la próxima captura entrega texturas sin copiarlas a CPU
    /// (`COROOM_ZERO_COPY=0` lo impide).
    fn set_gpu_frames(&mut self, frames: GpuFrames) -> GpuFrames {
        let disabled = std::env::var("COROOM_ZERO_COPY").is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "no" | "false"
            )
        });
        self.gpu = frames == GpuFrames::D3D11 && !disabled;
        if self.gpu {
            GpuFrames::D3D11
        } else {
            GpuFrames::Off
        }
    }

    /// Intervalo mínimo entre fotogramas y, con `hdr`, captura en `R16G16B16A16Float`.
    fn set_options(&mut self, options: CaptureOptions) -> CaptureOptions {
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
