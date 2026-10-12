//! Comprueba la build de FFmpeg antes de que la enlace `rusty_ffmpeg` (mensaje en español con lo que
//! hay que ejecutar si falta), añade el `rpath` de desarrollo en Linux y, en Windows, copia las DLL
//! junto a los ejecutables de `target/<perfil>/`. Ver `build/ffmpeg_dirs.rs`.

include!("build/ffmpeg_dirs.rs");

/// Copia las DLL de `<lib>/../bin` a `target/<perfil>/` y a `target/<perfil>/deps/`, donde están
/// los binarios y los tests, para que Windows las encuentre sin tocar el `PATH`.
fn copy_windows_dlls(lib: &Path) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let bin = lib.parent().map(|root| root.join("bin"));
    let Some(bin) = bin.filter(|bin| bin.is_dir()) else {
        println!(
            "cargo:warning=no se encontró el directorio bin de FFmpeg junto a {}",
            lib.display()
        );
        return;
    };
    let Some(profile) = std::env::var_os("OUT_DIR")
        .map(std::path::PathBuf::from)
        .and_then(|out| out.ancestors().nth(3).map(Path::to_path_buf))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&bin) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
        {
            continue;
        }
        for dir in [profile.clone(), profile.join("deps")] {
            let _ = std::fs::create_dir_all(&dir);
            if let Err(err) = std::fs::copy(&path, dir.join(entry.file_name())) {
                println!(
                    "cargo:warning=no se pudo copiar {} a {}: {err}",
                    path.display(),
                    dir.display()
                );
            }
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build/ffmpeg_dirs.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FFMPEG");
    if std::env::var_os("CARGO_FEATURE_FFMPEG").is_none() {
        return;
    }
    match ffmpeg_dirs() {
        Ok(dirs) => {
            emit_rpath(&dirs.lib);
            println!(
                "cargo:rustc-env=COROOM_FFMPEG_LIB_DIR={}",
                dirs.lib.display()
            );
            copy_windows_dlls(&dirs.lib);
        }
        Err(problem) => {
            eprintln!("{}", ffmpeg_help(&problem));
            std::process::exit(1);
        }
    }
}
