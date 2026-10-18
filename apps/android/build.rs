//! `rpath` de desarrollo a las bibliotecas de FFmpeg para las pruebas de este paquete en el equipo
//! de desarrollo, donde `coroom-session` lleva FFmpeg (en Android no hay FFmpeg y no hace falta).

include!("../../crates/codec/build/ffmpeg_dirs.rs");

fn main() {
    println!("cargo:rerun-if-changed=../../crates/codec/build/ffmpeg_dirs.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        return;
    }
    if let Ok(dirs) = ffmpeg_dirs() {
        emit_rpath(&dirs.lib);
    }
}
