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
//! - el ejecutable de coroom (ruta, tamaño y fecha) y la versión y el directorio de FFmpeg;
//! - las GPU (`vendor`/`device` de PCI de cada tarjeta de `/sys/class/drm`), el núcleo y la
//!   versión del módulo de NVIDIA;
//! - los drivers de Vulkan (cada manifiesto de `icd.d` y la biblioteca a la que apunta), el
//!   cargador `libvulkan.so.1` y los drivers de VA-API (`*_drv_video.so`), con tamaño y fecha;
//! - las variables de entorno que eligen drivers (`VK_ICD_FILENAMES`, `LIBVA_DRIVER_NAME`…);
//! - en Flatpak, `/.flatpak-info`, con los commits de la aplicación, del runtime y de sus
//!   extensiones (los drivers de `GL`): allí los ficheros tienen fecha 0.
//!
//! Solo en Linux; en el resto, `None` y el sondeo se hace siempre, como antes.

#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

/// Huella de esta máquina, o `None` donde no se calcula.
pub fn current() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::current())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fmt::Write as _;
    use std::time::UNIX_EPOCH;

    use super::{Path, PathBuf};

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
        let _ = writeln!(out, "kernel {}", read("/proc/sys/kernel/osrelease"));
        let _ = writeln!(out, "nvidia {}", read("/sys/module/nvidia/version"));
        let _ = writeln!(out, "flatpak {}", read("/.flatpak-info"));
        for gpu in gpus() {
            let _ = writeln!(out, "gpu {gpu}");
        }
        for var in ENV {
            if let Some(value) = std::env::var_os(var) {
                let _ = writeln!(out, "env {var}={}", value.to_string_lossy());
            }
        }
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
        for var in ["VK_ICD_FILENAMES", "VK_DRIVER_FILES", "VK_ADD_DRIVER_FILES"] {
            if let Some(value) = std::env::var_os(var) {
                files.extend(std::env::split_paths(&value));
            }
        }
        files.sort();
        files.dedup();
        files
    }

    /// Biblioteca a la que apunta un manifiesto (`"library_path"`): absoluta, relativa al
    /// manifiesto o buscada por nombre en [`LIB_DIRS`] y `LD_LIBRARY_PATH`.
    fn manifest_library(manifest: &Path) -> Option<PathBuf> {
        let text = std::fs::read_to_string(manifest).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        let library = json.get("ICD")?.get("library_path")?.as_str()?;
        let path = Path::new(library);
        if path.is_absolute() {
            return Some(path.to_path_buf());
        }
        if library.contains('/') {
            return manifest.parent().map(|dir| dir.join(path));
        }
        find_libs(|name| name == library).into_iter().next()
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

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "linux")]
    fn the_fingerprint_is_stable_and_names_the_executable() {
        let first = super::current().expect("en Linux hay huella");
        assert_eq!(super::current().as_deref(), Some(first.as_str()));
        assert!(first.starts_with("coroom "), "{first}");
        assert!(first.contains("\nffmpeg "), "{first}");
    }
}
