//! Capturas de todas las pantallas en claro y en oscuro, fuera de pantalla.
//!
//! Se guardan en PNG en `COROOM_SHOTS_DIR` (si está definida) o en un directorio temporal. Sin
//! adaptador gráfico (ni siquiera uno por software como `lavapipe`) el test se salta con un aviso.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use coroom_desktop::ui::shots::{SIZE, ShotRenderer, scenes};

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    let file = File::create(path).expect("fichero de la captura");
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("cabecera PNG");
    writer.write_image_data(rgba).expect("datos PNG");
}

/// Colores distintos (de una muestra): una pantalla vacía o sin texto tiene muy pocos.
fn distinct_colors(rgba: &[u8]) -> usize {
    let mut seen = std::collections::HashSet::new();
    for pixel in rgba.as_chunks::<4>().0.iter().step_by(7) {
        seen.insert([pixel[0], pixel[1], pixel[2]]);
    }
    seen.len()
}

#[test]
fn every_screen_renders_in_light_and_dark() {
    let mut renderer = match ShotRenderer::new() {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("sin capturas: {err}");
            return;
        }
    };
    let temp = tempfile::tempdir().expect("directorio temporal");
    let dir = std::env::var_os("COROOM_SHOTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().to_owned());
    std::fs::create_dir_all(&dir).expect("directorio de capturas");
    let scenes = scenes();
    assert_eq!(scenes.len() % 2, 0, "cada escena en claro y en oscuro");
    for scene in &scenes {
        let (width, height, rgba) = renderer.render(&scene.app, 1.0);
        assert_eq!((width, height), SIZE);
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        let colors = distinct_colors(&rgba);
        assert!(
            colors > 40,
            "{}: la captura parece vacía ({colors} colores)",
            scene.name
        );
        write_png(
            &dir.join(format!("{}.png", scene.name)),
            width,
            height,
            &rgba,
        );
    }
    // Una en HiDPI (escala 2): el texto y los iconos se rasterizan a la resolución física.
    let (width, height, rgba) = renderer.render(&scenes[4].app, 2.0);
    assert_eq!((width, height), (SIZE.0 * 2, SIZE.1 * 2));
    write_png(
        &dir.join(format!("{}-hidpi.png", scenes[4].name)),
        width,
        height,
        &rgba,
    );
    eprintln!("{} capturas en {}", scenes.len() + 1, dir.display());
}
