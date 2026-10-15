//! Huella de lo que decide el sondeo de códecs ([`crate::Capabilities::probe`]) en esta máquina,
//! para guardarlo en disco y no repetirlo en cada arranque.
//!
//! El sondeo abre cada codificador y decodificador por hardware y crea un dispositivo de la GPU
//! en cada apertura; en una RTX 5070 Ti con Linux son unos 15 dispositivos Vulkan de ~0,7 s, que
//! el driver no deja crear en paralelo, y el sondeo tarda unos 13 s. Quien abría una sala al poco
//! de arrancar esperaba todo eso sin saberlo. El resultado solo cambia si cambia algo de lo que
//! sigue, y eso es la huella (sin abrir nada de la GPU, así que un cargador Vulkan roto no la
//! tumba):
//!
//! - en todas las plataformas, el ejecutable de coroom (ruta, tamaño y fecha), la versión y el
//!   directorio de FFmpeg y las variables de entorno que eligen drivers (`VK_ICD_FILENAMES`…);
//! - en Linux, las GPU (`vendor`/`device` de PCI de cada tarjeta de `/sys/class/drm`), el núcleo y
//!   la versión del módulo de NVIDIA; los drivers de Vulkan (cada manifiesto de `icd.d` y la
//!   biblioteca a la que apunta), el cargador `libvulkan.so.1` y los drivers de VA-API
//!   (`*_drv_video.so`), con tamaño y fecha; y, en Flatpak, `/.flatpak-info`, con los commits de la
//!   aplicación, del runtime y de sus extensiones (los drivers de `GL`): allí los ficheros tienen
//!   fecha 0;
//! - en Windows, la versión del sistema (`CurrentBuild`, `UBR` y `DisplayVersion` del registro);
//!   cada adaptador de DXGI (descripción, `VendorId`, `DeviceId`, `SubSysId`, `Revision`,
//!   `Flags`) con la versión de su driver en modo usuario (`CheckInterfaceSupport`, que no crea
//!   ningún dispositivo); los paquetes de driver de la clase de adaptadores de pantalla
//!   (`DriverVersion` y `DriverDate`); los manifiestos de drivers de Vulkan del registro
//!   (`HKLM\SOFTWARE\Khronos\Vulkan\Drivers` y `VulkanDriverName` de cada adaptador) y la DLL a la
//!   que apuntan; y las DLL que abre el sondeo (`vulkan-1.dll`, `nvEncodeAPI64.dll`,
//!   `nvcuvid.dll`, `nvcuda.dll`, `amfrt64.dll`, `libmfx64-gen.dll`, `libmfxhw64.dll`,
//!   `libvpl.dll`, `d3d11.dll`, `d3d12.dll`), con tamaño y fecha, buscadas en `System32` y en los
//!   directorios de las DLL de Vulkan; las que no están se anotan como ausentes. No entra el LUID
//!   del adaptador, que cambia en cada arranque del sistema y dejaría la caché sin uso.
//!
//! Si una parte falla (sin DXGI, clave del registro ausente), la huella anota el error y sigue:
//! nunca impide arrancar. En otras plataformas, `None` y el sondeo se hace siempre, como antes.

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Huella de esta máquina, o `None` donde no se calcula.
pub fn current() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::current())
    }
    #[cfg(target_os = "windows")]
    {
        Some(windows::current())
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Primeras líneas de la huella, iguales en todas las plataformas: el ejecutable y FFmpeg.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn write_header(out: &mut String) {
    let _ = writeln!(
        out,
        "coroom {}",
        file_id(std::env::current_exe().ok().as_deref())
    );
    let _ = writeln!(
        out,
        "ffmpeg {} {}",
        crate::ffmpeg::version(),
        crate::ffmpeg::library_dir()
    );
}

/// Una línea `env VAR=valor` por cada variable de `vars` que esté definida.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn write_env(out: &mut String, vars: &[&str]) {
    for var in vars {
        if let Some(value) = std::env::var_os(var) {
            let _ = writeln!(out, "env {var}={}", value.to_string_lossy());
        }
    }
}

/// `ruta tamaño fecha` del fichero (siguiendo enlaces), o `ruta -` si no existe.
fn file_id(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return "-".to_owned();
    };
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            format!("{} {} {modified}", path.display(), meta.len())
        }
        Err(_) => format!("{} -", path.display()),
    }
}

/// `library_path` de un manifiesto de driver de Vulkan (`{"ICD": {"library_path": …}}`).
fn icd_library(manifest: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(manifest).ok()?;
    json.get("ICD")?
        .get("library_path")?
        .as_str()
        .map(str::to_owned)
}

/// Biblioteca a la que apunta el manifiesto `manifest` con `library_path = library`: absoluta,
/// relativa al directorio del manifiesto (si lleva separador) o buscada por nombre con `by_name`.
fn resolve_library(
    manifest: &Path,
    library: &str,
    by_name: impl FnOnce(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    let path = Path::new(library);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    if library.contains(['/', '\\']) {
        return manifest.parent().map(|dir| dir.join(path));
    }
    by_name(library)
}

/// Manifiestos de Vulkan dados por las variables de entorno del cargador (`VK_ICD_FILENAMES`,
/// `VK_DRIVER_FILES`, `VK_ADD_DRIVER_FILES`), separados como `PATH`.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn env_manifests() -> Vec<PathBuf> {
    ["VK_ICD_FILENAMES", "VK_DRIVER_FILES", "VK_ADD_DRIVER_FILES"]
        .into_iter()
        .filter_map(std::env::var_os)
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .collect()
}

/// Versión de un driver en modo usuario tal y como la da `IDXGIAdapter::CheckInterfaceSupport`
/// (un `LARGE_INTEGER` con cuatro palabras de 16 bits: `32.0.15.6094`).
#[cfg(any(test, target_os = "windows"))]
fn umd_version(version: i64) -> String {
    let bits = version as u64;
    format!(
        "{}.{}.{}.{}",
        (bits >> 48) & 0xffff,
        (bits >> 32) & 0xffff,
        (bits >> 16) & 0xffff,
        bits & 0xffff
    )
}

/// Texto UTF-16 hasta el primer NUL (una `Description` de DXGI o un `REG_SZ`).
#[cfg(any(test, target_os = "windows"))]
fn utf16_until_nul(text: &[u16]) -> String {
    let end = text
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end])
}

/// Unidades UTF-16 de los bytes de un valor del registro (`REG_SZ`, `REG_MULTI_SZ`), sin el
/// byte suelto del final si lo hubiera.
#[cfg(any(test, target_os = "windows"))]
fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect()
}

/// Cadenas de un `REG_MULTI_SZ` (UTF-16 en bytes, cada una terminada en NUL y otro NUL al final).
#[cfg(any(test, target_os = "windows"))]
fn reg_multi_sz(bytes: &[u8]) -> Vec<String> {
    utf16_units(bytes)
        .split(|&unit| unit == 0)
        .filter(|text| !text.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fmt::Write as _;

    use super::{Path, PathBuf, file_id};

    /// Variables de entorno que cambian qué driver se carga.
    const ENV: [&str; 8] = [
        "VK_ICD_FILENAMES",
        "VK_DRIVER_FILES",
        "VK_ADD_DRIVER_FILES",
        "LIBVA_DRIVER_NAME",
        "LIBVA_DRIVERS_PATH",
        "LD_LIBRARY_PATH",
        "__EGL_VENDOR_LIBRARY_FILENAMES",
        "COROOM_PROBE_IN_PROCESS",
    ];

    /// Directorios donde se buscan bibliotecas por nombre (manifiestos con `library_path` sin
    /// ruta, el cargador de Vulkan y los drivers de VA-API).
    const LIB_DIRS: [&str; 7] = [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/lib64",
        "/app/lib",
        "/usr/lib/x86_64-linux-gnu/GL/default/lib",
    ];

    pub(super) fn current() -> String {
        let mut out = String::new();
        super::write_header(&mut out);
        let _ = writeln!(out, "kernel {}", read("/proc/sys/kernel/osrelease"));
        let _ = writeln!(out, "nvidia {}", read("/sys/module/nvidia/version"));
        let _ = writeln!(out, "flatpak {}", read("/.flatpak-info"));
        for gpu in gpus() {
            let _ = writeln!(out, "gpu {gpu}");
        }
        super::write_env(&mut out, &ENV);
        for manifest in vulkan_manifests() {
            let _ = writeln!(out, "icd {}", file_id(Some(&manifest)));
            if let Some(library) = manifest_library(&manifest) {
                let _ = writeln!(out, "icd-lib {}", file_id(Some(&library)));
            }
        }
        for library in find_libs(|name| name == "libvulkan.so.1") {
            let _ = writeln!(out, "loader {}", file_id(Some(&library)));
        }
        for library in va_drivers() {
            let _ = writeln!(out, "va {}", file_id(Some(&library)));
        }
        out
    }

    fn read(path: &str) -> String {
        std::fs::read_to_string(path)
            .map(|text| text.trim().to_owned())
            .unwrap_or_default()
    }

    /// `vendor:device` de PCI de cada tarjeta, ordenadas.
    fn gpus() -> Vec<String> {
        let mut gpus: Vec<String> = std::fs::read_dir("/sys/class/drm")
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("card") && !name.contains('-')
            })
            .map(|entry| {
                let device = entry.path().join("device");
                let id = |file: &str| {
                    std::fs::read_to_string(device.join(file))
                        .map(|text| text.trim().to_owned())
                        .unwrap_or_default()
                };
                format!("{}:{}", id("vendor"), id("device"))
            })
            .collect();
        gpus.sort();
        gpus.dedup();
        gpus
    }

    /// Manifiestos de drivers de Vulkan (`icd.d/*.json`) de los directorios estándar, de
    /// `XDG_DATA_DIRS`/`XDG_CONFIG_DIRS` y de las variables de entorno, ordenados.
    fn vulkan_manifests() -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for var in ["XDG_CONFIG_DIRS", "XDG_DATA_DIRS"] {
            if let Some(value) = std::env::var_os(var) {
                dirs.extend(std::env::split_paths(&value).map(|dir| dir.join("vulkan/icd.d")));
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".local/share/vulkan/icd.d"));
        }
        for dir in [
            "/etc/vulkan/icd.d",
            "/usr/share/vulkan/icd.d",
            "/usr/local/share/vulkan/icd.d",
        ] {
            dirs.push(PathBuf::from(dir));
        }
        let mut files: Vec<PathBuf> = dirs
            .iter()
            .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.extend(super::env_manifests());
        files.sort();
        files.dedup();
        files
    }

    /// Biblioteca a la que apunta un manifiesto (`"library_path"`): absoluta, relativa al
    /// manifiesto o buscada por nombre en [`LIB_DIRS`] y `LD_LIBRARY_PATH`.
    fn manifest_library(manifest: &Path) -> Option<PathBuf> {
        let text = std::fs::read_to_string(manifest).ok()?;
        let library = super::icd_library(&text)?;
        super::resolve_library(manifest, &library, |name| {
            find_libs(|candidate| candidate == name).into_iter().next()
        })
    }

    /// Directorios de bibliotecas: `LD_LIBRARY_PATH` y [`LIB_DIRS`].
    fn lib_dirs() -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::env::var_os("LD_LIBRARY_PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        dirs.extend(LIB_DIRS.iter().map(PathBuf::from));
        dirs
    }

    /// Ficheros de los directorios de bibliotecas cuyo nombre cumple `matches`.
    fn find_libs(matches: impl Fn(&str) -> bool) -> Vec<PathBuf> {
        lib_dirs()
            .iter()
            .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
            .filter(|entry| matches(&entry.file_name().to_string_lossy()))
            .map(|entry| entry.path())
            .collect()
    }

    /// Drivers de VA-API (`*_drv_video.so`) de `LIBVA_DRIVERS_PATH` o de los `dri` estándar.
    fn va_drivers() -> Vec<PathBuf> {
        let dirs: Vec<PathBuf> = match std::env::var_os("LIBVA_DRIVERS_PATH") {
            Some(value) => std::env::split_paths(&value).collect(),
            None => lib_dirs().iter().map(|dir| dir.join("dri")).collect(),
        };
        let mut files: Vec<PathBuf> = dirs
            .iter()
            .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with("_drv_video.so"))
            })
            .collect();
        files.sort();
        files
    }
}

/// Huella de Windows: DXGI (sin crear dispositivos), el registro y las DLL de los drivers. Las
/// llamadas a Win32 van en `unsafe` con su invariante; ninguna abre la GPU ni carga un driver.
#[cfg(target_os = "windows")]
mod windows {
    use std::fmt::Write as _;

    use ::windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
    };
    use ::windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice,
        IDXGIFactory1,
    };
    use ::windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_DWORD, REG_EXPAND_SZ,
        REG_MULTI_SZ, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegEnumKeyExW, RegEnumValueW,
        RegOpenKeyExW, RegQueryValueExW,
    };
    use ::windows::core::{Interface, PCWSTR, PWSTR};

    use super::{Path, PathBuf, file_id};

    /// Variables de entorno que cambian qué driver carga el cargador de Vulkan o el sondeo.
    const ENV: [&str; 6] = [
        "VK_ICD_FILENAMES",
        "VK_DRIVER_FILES",
        "VK_ADD_DRIVER_FILES",
        "VK_LOADER_DRIVERS_SELECT",
        "VK_LOADER_DRIVERS_DISABLE",
        "COROOM_PROBE_IN_PROCESS",
    ];

    /// DLL que abre el sondeo: el cargador de Vulkan, NVENC y NVDEC (con CUDA), AMF, QSV (el
    /// runtime de Intel nuevo y el viejo, y el despachador de VPL) y Direct3D.
    const DLLS: [&str; 10] = [
        "vulkan-1.dll",
        "nvEncodeAPI64.dll",
        "nvcuvid.dll",
        "nvcuda.dll",
        "amfrt64.dll",
        "libmfx64-gen.dll",
        "libmfxhw64.dll",
        "libvpl.dll",
        "d3d11.dll",
        "d3d12.dll",
    ];

    /// Versión del sistema.
    const WINDOWS_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    /// Manifiestos de drivers de Vulkan registrados a la antigua: cada valor es la ruta de un
    /// manifiesto y su dato (`REG_DWORD`) es 0 si está activo.
    const VULKAN_DRIVERS_KEY: &str = r"SOFTWARE\Khronos\Vulkan\Drivers";

    /// Clase de adaptadores de pantalla: una subclave `0000`, `0001`… por adaptador con el
    /// paquete de driver instalado y, en los drivers modernos, `VulkanDriverName`.
    const DISPLAY_CLASS_KEY: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

    pub(super) fn current() -> String {
        let mut out = String::new();
        super::write_header(&mut out);
        let _ = writeln!(out, "windows {}", windows_version());
        match gpus() {
            Ok(gpus) => {
                for gpu in gpus {
                    let _ = writeln!(out, "gpu {gpu}");
                }
            }
            Err(err) => {
                let _ = writeln!(out, "gpu-error {err}");
            }
        }
        for driver in display_drivers() {
            let _ = writeln!(out, "driver {driver}");
        }
        super::write_env(&mut out, &ENV);
        let mut lib_dirs = vec![system32()];
        for manifest in vulkan_manifests(&mut out) {
            let _ = writeln!(out, "icd {}", file_id(Some(&manifest)));
            if let Some(library) = manifest_library(&manifest) {
                let _ = writeln!(out, "icd-lib {}", file_id(Some(&library)));
                if let Some(dir) = library.parent()
                    && !lib_dirs.iter().any(|known| known == dir)
                {
                    lib_dirs.push(dir.to_path_buf());
                }
            }
        }
        for name in DLLS {
            let mut found = false;
            for dir in &lib_dirs {
                let path = dir.join(name);
                if path.is_file() {
                    found = true;
                    let _ = writeln!(out, "dll {}", file_id(Some(&path)));
                }
            }
            if !found {
                let _ = writeln!(out, "dll {name} -");
            }
        }
        out
    }

    /// `%SystemRoot%\System32`, donde instalan sus DLL los drivers de NVIDIA, AMD e Intel.
    fn system32() -> PathBuf {
        let root = std::env::var_os("SystemRoot")
            .or_else(|| std::env::var_os("windir"))
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        root.join("System32")
    }

    /// `CurrentBuild.UBR (DisplayVersion)` del registro, o el error.
    fn windows_version() -> String {
        match Key::open(WINDOWS_VERSION_KEY) {
            Ok(key) => format!(
                "{}.{} ({})",
                key.value_text("CurrentBuild"),
                key.value_text("UBR"),
                key.value_text("DisplayVersion")
            ),
            Err(err) => format!("error {err}"),
        }
    }

    /// Cada adaptador de DXGI: descripción, ids PCI, flags y versión del driver en modo usuario.
    /// No crea ningún dispositivo: `CheckInterfaceSupport` solo consulta la versión del UMD.
    fn gpus() -> Result<Vec<String>, String> {
        // SAFETY: `CreateDXGIFactory1` no recibe punteros nuestros y devuelve una interfaz COM
        // con cuenta de referencias que `IDXGIFactory1` libera al soltarse.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.map_err(|err| format!("CreateDXGIFactory1: {err}"))?;
        let mut gpus = Vec::new();
        for index in 0.. {
            // SAFETY: `factory` es una fábrica válida; el adaptador devuelto lleva su cuenta de
            // referencias.
            let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => return Err(format!("EnumAdapters1({index}): {err}")),
            };
            // SAFETY: `adapter` es un adaptador válido; `GetDesc1` escribe en una estructura local.
            let desc =
                unsafe { adapter.GetDesc1() }.map_err(|err| format!("GetDesc1({index}): {err}"))?;
            let base: &IDXGIAdapter = (&adapter).into();
            // SAFETY: el GUID apunta a la constante `IID` de `IDXGIDevice`, válida toda la
            // llamada; DXGI solo lee la versión del UMD, sin cargarlo ni crear dispositivos.
            let umd = match unsafe { base.CheckInterfaceSupport(&IDXGIDevice::IID) } {
                Ok(version) => super::umd_version(version),
                Err(err) => format!("error:{}", err.code().0),
            };
            gpus.push(format!(
                "{} {:04x}:{:04x}:{:08x}:{:02x} flags={} umd={umd}",
                super::utf16_until_nul(&desc.Description),
                desc.VendorId,
                desc.DeviceId,
                desc.SubSysId,
                desc.Revision,
                desc.Flags,
            ));
        }
        Ok(gpus)
    }

    /// `subclave versión fecha` de cada paquete de driver de la clase de adaptadores de pantalla.
    fn display_drivers() -> Vec<String> {
        let Ok(class) = Key::open(DISPLAY_CLASS_KEY) else {
            return Vec::new();
        };
        class
            .subkeys()
            .into_iter()
            .filter_map(|name| {
                let key = Key::open(&format!(r"{DISPLAY_CLASS_KEY}\{name}")).ok()?;
                Some(format!(
                    "{name} {} {}",
                    key.value_text("DriverVersion"),
                    key.value_text("DriverDate")
                ))
            })
            .collect()
    }

    /// Manifiestos de drivers de Vulkan: los de `HKLM\SOFTWARE\Khronos\Vulkan\Drivers` (con su
    /// estado), los `VulkanDriverName` de cada adaptador de pantalla y los de las variables de
    /// entorno, ordenados. Los errores del registro se anotan en `out`.
    fn vulkan_manifests(out: &mut String) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = Vec::new();
        match Key::open(VULKAN_DRIVERS_KEY) {
            Ok(key) => {
                for name in key.value_names() {
                    let _ = writeln!(out, "icd-reg {name}={}", key.value_text(&name));
                    files.push(PathBuf::from(name));
                }
            }
            Err(err) => {
                let _ = writeln!(out, "icd-reg-error {err}");
            }
        }
        if let Ok(class) = Key::open(DISPLAY_CLASS_KEY) {
            for name in class.subkeys() {
                if let Ok(key) = Key::open(&format!(r"{DISPLAY_CLASS_KEY}\{name}")) {
                    files.extend(
                        key.value_strings("VulkanDriverName")
                            .into_iter()
                            .map(PathBuf::from),
                    );
                }
            }
        }
        files.extend(super::env_manifests());
        files.sort();
        files.dedup();
        files
    }

    /// DLL a la que apunta un manifiesto (`"library_path"`): absoluta, relativa al manifiesto o
    /// buscada por nombre en `System32`.
    fn manifest_library(manifest: &Path) -> Option<PathBuf> {
        let text = std::fs::read_to_string(manifest).ok()?;
        let library = super::icd_library(&text)?;
        super::resolve_library(manifest, &library, |name| {
            let path = system32().join(name);
            path.is_file().then_some(path)
        })
    }

    /// Cadena UTF-16 terminada en NUL para pasarla a Win32.
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Clave del registro abierta solo para leer; se cierra al soltarse.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: `self.0` la abrió `RegOpenKeyExW` y nadie la ha cerrado antes.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    impl Key {
        /// Abre `HKLM\path` en la vista de 64 bits del registro.
        fn open(path: &str) -> Result<Self, String> {
            let wide = wide(path);
            let mut key = HKEY::default();
            // SAFETY: `wide` es una cadena terminada en NUL que vive toda la llamada y `key` es
            // una variable local donde se escribe el manejador.
            let status = unsafe {
                RegOpenKeyExW(
                    HKEY_LOCAL_MACHINE,
                    PCWSTR(wide.as_ptr()),
                    None,
                    KEY_READ | KEY_WOW64_64KEY,
                    &mut key,
                )
            };
            status.ok().map_err(|err| format!("{path}: {err}"))?;
            Ok(Self(key))
        }

        /// Nombres de las subclaves, en el orden del registro.
        fn subkeys(&self) -> Vec<String> {
            let mut names = Vec::new();
            // Una subclave tiene como mucho 255 caracteres, más el NUL.
            let mut buffer = [0u16; 256];
            for index in 0.. {
                let mut len = buffer.len() as u32;
                // SAFETY: `buffer` y `len` son locales del tamaño que se declara; el resto de
                // salidas se omiten (`None`) y la API lo admite.
                let status = unsafe {
                    RegEnumKeyExW(
                        self.0,
                        index,
                        Some(PWSTR(buffer.as_mut_ptr())),
                        &mut len,
                        None,
                        None,
                        None,
                        None,
                    )
                };
                if status == ERROR_NO_MORE_ITEMS || status.is_err() {
                    break;
                }
                names.push(String::from_utf16_lossy(&buffer[..len as usize]));
            }
            names
        }

        /// Nombres de los valores, en el orden del registro.
        fn value_names(&self) -> Vec<String> {
            let mut names = Vec::new();
            // Un nombre de valor tiene como mucho 16 383 caracteres, más el NUL.
            let mut buffer = vec![0u16; 16_384];
            for index in 0.. {
                let mut len = buffer.len() as u32;
                // SAFETY: `buffer` y `len` son locales del tamaño que se declara; sin pedir los
                // datos (`None`) solo se escribe el nombre.
                let status = unsafe {
                    RegEnumValueW(
                        self.0,
                        index,
                        Some(PWSTR(buffer.as_mut_ptr())),
                        &mut len,
                        None,
                        None,
                        None,
                        None,
                    )
                };
                if status == ERROR_NO_MORE_ITEMS || status.is_err() {
                    break;
                }
                names.push(String::from_utf16_lossy(&buffer[..len as usize]));
            }
            names
        }

        /// Tipo y bytes de un valor; `None` si no existe y el error de Win32 si falla la lectura.
        fn value(&self, name: &str) -> Result<Option<(REG_VALUE_TYPE, Vec<u8>)>, String> {
            let wide = wide(name);
            let mut kind = REG_VALUE_TYPE::default();
            let mut size = 0u32;
            // SAFETY: sin búfer de datos, la API solo escribe el tipo y el tamaño en las
            // variables locales que se le pasan.
            let status = unsafe {
                RegQueryValueExW(
                    self.0,
                    PCWSTR(wide.as_ptr()),
                    None,
                    Some(&raw mut kind),
                    None,
                    Some(&raw mut size),
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            status.ok().map_err(|err| format!("{name}: {err}"))?;
            let mut data = vec![0u8; size as usize];
            // SAFETY: `data` tiene `size` bytes, el tamaño que acaba de dar la API, y `size` dice
            // cuántos se escriben; si el valor creció entre medias devuelve `ERROR_MORE_DATA`.
            let status = unsafe {
                RegQueryValueExW(
                    self.0,
                    PCWSTR(wide.as_ptr()),
                    None,
                    Some(&raw mut kind),
                    Some(data.as_mut_ptr()),
                    Some(&raw mut size),
                )
            };
            if status == ERROR_MORE_DATA {
                return Err(format!("{name}: el valor cambió al leerlo"));
            }
            status.ok().map_err(|err| format!("{name}: {err}"))?;
            data.truncate(size as usize);
            Ok(Some((kind, data)))
        }

        /// Un valor como texto: `REG_SZ`/`REG_EXPAND_SZ` tal cual, `REG_MULTI_SZ` unido con `;`,
        /// `REG_DWORD` en decimal, otros tipos como `tipo:bytes` en hexadecimal; `-` si no está y
        /// `error …` si falla la lectura.
        fn value_text(&self, name: &str) -> String {
            match self.value(name) {
                Ok(None) => "-".to_owned(),
                Ok(Some((kind, data))) if kind == REG_SZ || kind == REG_EXPAND_SZ => {
                    utf16_bytes(&data)
                }
                Ok(Some((kind, data))) if kind == REG_MULTI_SZ => {
                    super::reg_multi_sz(&data).join(";")
                }
                Ok(Some((kind, data))) if kind == REG_DWORD && data.len() == 4 => {
                    u32::from_le_bytes([data[0], data[1], data[2], data[3]]).to_string()
                }
                Ok(Some((kind, data))) => {
                    let mut text = format!("{}:", kind.0);
                    for byte in data {
                        let _ = write!(text, "{byte:02x}");
                    }
                    text
                }
                Err(err) => format!("error {err}"),
            }
        }

        /// Las cadenas de un valor `REG_MULTI_SZ`, o la única de un `REG_SZ`; vacío si no está.
        fn value_strings(&self, name: &str) -> Vec<String> {
            match self.value(name) {
                Ok(Some((kind, data))) if kind == REG_MULTI_SZ => super::reg_multi_sz(&data),
                Ok(Some((kind, data))) if kind == REG_SZ || kind == REG_EXPAND_SZ => {
                    let text = utf16_bytes(&data);
                    if text.is_empty() {
                        Vec::new()
                    } else {
                        vec![text]
                    }
                }
                _ => Vec::new(),
            }
        }
    }

    /// Texto UTF-16 en bytes (un `REG_SZ`) hasta el primer NUL.
    fn utf16_bytes(bytes: &[u8]) -> String {
        super::utf16_until_nul(&super::utf16_units(bytes))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn the_fingerprint_is_stable_and_names_the_executable() {
        let first = super::current().expect("en Linux y Windows hay huella");
        assert_eq!(super::current().as_deref(), Some(first.as_str()));
        assert!(first.starts_with("coroom "), "{first}");
        assert!(first.contains("\nffmpeg "), "{first}");
    }

    #[test]
    fn file_id_marks_missing_files() {
        assert_eq!(super::file_id(None), "-");
        let missing = Path::new("/definitivamente/no/existe.dll");
        assert_eq!(
            super::file_id(Some(missing)),
            format!("{} -", missing.display())
        );
    }

    #[test]
    fn icd_manifests_from_linux_and_windows_are_parsed() {
        let linux = r#"{"file_format_version":"1.0.1","ICD":{"library_path":"libvulkan_nvidia.so.0","api_version":"1.4.303"}}"#;
        assert_eq!(
            super::icd_library(linux).as_deref(),
            Some("libvulkan_nvidia.so.0")
        );
        let windows = r#"{"file_format_version":"1.0.1","ICD":{"library_path":".\\nvoglv64.dll","api_version":"1.4.303","is_portability_driver":false}}"#;
        assert_eq!(
            super::icd_library(windows).as_deref(),
            Some(r".\nvoglv64.dll")
        );
        assert_eq!(super::icd_library(r#"{"ICD":{}}"#), None);
        assert_eq!(super::icd_library("no es json"), None);
    }

    #[test]
    fn libraries_resolve_relative_to_the_manifest_or_by_name() {
        let manifest = Path::new("/drivers/nvidia/nv_icd.json");
        let by_name = |name: &str| Some(PathBuf::from("/lib").join(name));
        assert_eq!(
            super::resolve_library(manifest, "libvulkan_nvidia.so.0", by_name),
            Some(PathBuf::from("/lib/libvulkan_nvidia.so.0"))
        );
        assert_eq!(
            super::resolve_library(manifest, r".\nvoglv64.dll", by_name),
            Some(PathBuf::from("/drivers/nvidia").join(r".\nvoglv64.dll"))
        );
        assert_eq!(
            super::resolve_library(manifest, "./libvulkan_radeon.so", by_name),
            Some(PathBuf::from("/drivers/nvidia/./libvulkan_radeon.so"))
        );
        assert_eq!(
            super::resolve_library(manifest, "/usr/lib/libvulkan_intel.so", by_name),
            Some(PathBuf::from("/usr/lib/libvulkan_intel.so"))
        );
        assert_eq!(super::resolve_library(manifest, "nada", |_| None), None);
    }

    #[test]
    fn the_umd_version_has_four_words() {
        // 32.0.15.6094, como lo da `CheckInterfaceSupport` para un driver de NVIDIA.
        let version = (32i64 << 48) | (15i64 << 16) | 6094;
        assert_eq!(super::umd_version(version), "32.0.15.6094");
        assert_eq!(super::umd_version(0), "0.0.0.0");
        assert_eq!(super::umd_version(-1), "65535.65535.65535.65535");
    }

    #[test]
    fn utf16_stops_at_the_first_nul() {
        let mut description = [0u16; 8];
        for (slot, unit) in description.iter_mut().zip("GPU".encode_utf16()) {
            *slot = unit;
        }
        assert_eq!(super::utf16_until_nul(&description), "GPU");
        assert_eq!(super::utf16_until_nul(&[]), "");
        let without_nul: Vec<u16> = "Radeon".encode_utf16().collect();
        assert_eq!(super::utf16_until_nul(&without_nul), "Radeon");
    }

    #[test]
    fn multi_sz_splits_on_nul_and_drops_the_terminator() {
        let mut bytes = Vec::new();
        for text in ["a.json", "b.json", ""] {
            for unit in text.encode_utf16().chain(std::iter::once(0)) {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
        }
        assert_eq!(super::reg_multi_sz(&bytes), ["a.json", "b.json"]);
        assert!(super::reg_multi_sz(&[]).is_empty());
        assert!(super::reg_multi_sz(&[0, 0]).is_empty());
    }
}
