//! `rpath` de desarrollo a las bibliotecas de FFmpeg para los binarios y tests de este paquete
//! (los argumentos de enlazado de `coroom-codec` no se heredan). Si falta FFmpeg, el error en
//! español lo da el `build.rs` de `coroom-codec`.

include!("../codec/build/ffmpeg_dirs.rs");

fn main() {
    println!("cargo:rerun-if-changed=../codec/build/ffmpeg_dirs.rs");
    if let Ok(dirs) = ffmpeg_dirs() {
        emit_rpath(&dirs.lib);
    }
}
