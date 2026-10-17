//! Capturas de todas las pantallas en claro y en oscuro, fuera de pantalla.
//!
//! Se guardan en PNG en `COROOM_SHOTS_DIR` si está definida; sin ella solo se comprueba que cada
//! pantalla se dibuje. Sin adaptador gráfico (ni siquiera uno por software como `lavapipe`) el test
//! se salta con un aviso.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use coroom_desktop::ui::shots::{NARROW, SIZE, ShotRenderer, WIDE, scenes};

fn write_png(dir: Option<&Path>, name: &str, width: u32, height: u32, rgba: &[u8]) {
    let Some(dir) = dir else { return };
    let file = File::create(dir.join(format!("{name}.png"))).expect("fichero de la captura");
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
    let dir = std::env::var_os("COROOM_SHOTS_DIR").map(PathBuf::from);
    if let Some(dir) = &dir {
        std::fs::create_dir_all(dir).expect("directorio de capturas");
    }
    let scenes = scenes();
    assert_eq!(scenes.len() % 2, 0, "cada escena en claro y en oscuro");
    for scene in &scenes {
        let (width, height, rgba) = renderer.render(&scene.app, 1.0);
        let window = (
            scene.app.window.width as u32,
            scene.app.window.height as u32,
        );
        assert_eq!((width, height), window);
        assert!([SIZE, NARROW, WIDE].contains(&window), "{}", scene.name);
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        let colors = distinct_colors(&rgba);
        assert!(
            colors > 40,
            "{}: la captura parece vacía ({colors} colores)",
            scene.name
        );
        write_png(dir.as_deref(), &scene.name, width, height, &rgba);
    }
    // Una en HiDPI (escala 2): el texto y los iconos se rasterizan a la resolución física.
    let (width, height, rgba) = renderer.render(&scenes[4].app, 2.0);
    assert_eq!((width, height), (SIZE.0 * 2, SIZE.1 * 2));
    let hidpi = format!("{}-hidpi", scenes[4].name);
    write_png(dir.as_deref(), &hidpi, width, height, &rgba);
    if let Some(dir) = &dir {
        eprintln!("{} capturas en {}", scenes.len() + 1, dir.display());
    }
}
