// Localización de FFmpeg compartida por los `build.rs` de `coroom-codec`, `coroom-session` y
// `coroom` (se incluye con `include!`). FFmpeg lo enlaza `rusty_ffmpeg` con las variables
// `FFMPEG_INCLUDE_DIR`, `FFMPEG_LIBS_DIR` y `FFMPEG_LINK_MODE`, que por defecto pone
// `.cargo/config.toml` apuntando a `third_party/ffmpeg/` (la build de `tools/ffmpeg.lock`); quien
// quiera otra build exporta esas variables, que tienen prioridad.
//
// Aquí se comprueba antes que `rusty_ffmpeg` falle con un mensaje en inglés: que las cabeceras y
// las bibliotecas existen y que son de FFmpeg ≥ 8.0 (libavcodec 62). En Linux se añade `rpath` al
// directorio de las bibliotecas para que `cargo test` y `cargo run` las encuentren sin
// `LD_LIBRARY_PATH` (solo desarrollo: Flatpak y MSIX empaquetan su propio FFmpeg).

use std::path::{Path, PathBuf};

/// Primera versión mayor de libavcodec de FFmpeg 8.0.
const MIN_LIBAVCODEC_MAJOR: u32 = 62;

/// Directorios de la build de FFmpeg con la que se enlaza.
#[allow(dead_code)]
struct FfmpegDirs {
    include: PathBuf,
    lib: PathBuf,
    libavcodec_major: u32,
}

#[allow(dead_code)]
fn ffmpeg_help(problem: &str) -> String {
    format!(
        "\n\nNo se puede compilar coroom con FFmpeg: {problem}.\n\n\
         coroom necesita FFmpeg 8 compartido (LGPL). Para descargar la build fijada en\n\
         tools/ffmpeg.lock en third_party/ffmpeg/, ejecuta desde la raíz del repositorio:\n\n\
         \x20   tools/fetch-ffmpeg.sh          (Linux)\n\
         \x20   tools\\fetch-ffmpeg.ps1         (Windows, PowerShell)\n\n\
         Para usar otra build de FFmpeg ≥ 8.0, exporta FFMPEG_INCLUDE_DIR (cabeceras) y\n\
         FFMPEG_LIBS_DIR (bibliotecas). Más detalles en README.md, «FFmpeg».\n"
    )
}

fn env_path(name: &str) -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Lee `LIBAVCODEC_VERSION_MAJOR` de las cabeceras.
fn libavcodec_major(include: &Path) -> Result<u32, String> {
    let header = include.join("libavcodec").join("version_major.h");
    println!("cargo:rerun-if-changed={}", header.display());
    let text = std::fs::read_to_string(&header).map_err(|_| {
        format!(
            "no hay cabeceras de FFmpeg en {} (falta libavcodec/version_major.h)",
            include.display()
        )
    })?;
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("#define LIBAVCODEC_VERSION_MAJOR"))
        .find_map(|value| value.trim().parse::<u32>().ok())
        .ok_or_else(|| format!("{} no declara LIBAVCODEC_VERSION_MAJOR", header.display()))
}

fn has_libavcodec(lib: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(lib) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        name.starts_with("libavcodec.so")
            || name == "libavcodec.dylib"
            || name == "avcodec.lib"
            || name == "libavcodec.dll.a"
    })
}

fn ffmpeg_dirs() -> Result<FfmpegDirs, String> {
    let include = env_path("FFMPEG_INCLUDE_DIR")
        .ok_or_else(|| "FFMPEG_INCLUDE_DIR no está definida (¿falta .cargo/config.toml?)".to_owned())?;
    let lib = env_path("FFMPEG_LIBS_DIR")
        .ok_or_else(|| "FFMPEG_LIBS_DIR no está definida (¿falta .cargo/config.toml?)".to_owned())?;
    let libavcodec_major = libavcodec_major(&include)?;
    if libavcodec_major < MIN_LIBAVCODEC_MAJOR {
        return Err(format!(
            "las cabeceras de {} son de libavcodec {libavcodec_major}, anterior a FFmpeg 8.0 \
             (libavcodec {MIN_LIBAVCODEC_MAJOR})",
            include.display()
        ));
    }
    println!("cargo:rerun-if-changed={}", lib.display());
    if !has_libavcodec(&lib) {
        return Err(format!("no hay bibliotecas de FFmpeg en {}", lib.display()));
    }
    let absolute = |path: PathBuf| std::fs::canonicalize(&path).unwrap_or(path);
    Ok(FfmpegDirs {
        include: absolute(include),
        lib: absolute(lib),
        libavcodec_major,
    })
}

/// `rpath` al directorio de las bibliotecas en los binarios y tests de este paquete (Linux y
/// otros Unix; en Windows las DLL van junto al ejecutable o en el `PATH`).
///
/// Se emite como `DT_RPATH` clásico (`--disable-new-dtags`) y no como `DT_RUNPATH`: el cargador
/// solo aplica `RUNPATH` a las dependencias directas del ejecutable, y las builds de BtbN no llevan
/// `rpath` propio, así que `libswresample` y `libswscale` (dependencias de `libavcodec`, no del
/// binario) no se encontrarían. `DT_RPATH` sí vale para toda la cadena.
///
/// Nunca a un directorio del sistema (`/usr`, `/lib`): el cargador ya lo busca, y un `DT_RPATH`
/// ahí pasa por delante de las rutas que añade el runtime (en el Flatpak 1.6.2 se saltaba la
/// extensión `codecs-extra` y cargaba el FFmpeg recortado de la Platform).
fn emit_rpath(lib: &Path) {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let system = ["/usr", "/lib", "/lib64"].iter().any(|dir| lib.starts_with(dir));
    if target_os != "windows" && target_os != "macos" && !system {
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
    }
}
