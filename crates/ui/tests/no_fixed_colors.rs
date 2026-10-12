//! Ningún color fijo fuera de `crates/ui/src/theme.rs`: ni en el sistema de diseño ni en la
//! interfaz de la aplicación (`apps/desktop/src/ui`). Todo color sale de la paleta del tema, así
//! que el modo claro y oscuro, el acento del sistema y el contraste AA valen en todas partes.

use std::fs;
use std::path::{Path, PathBuf};

/// Lo que delata un color escrito a mano.
const FORBIDDEN: [&str; 10] = [
    "Color::from_rgb",
    "Color::from_rgba",
    "Color::new(",
    "Color::BLACK",
    "Color::WHITE",
    "Color::from_linear_rgba",
    "color!(",
    "Rgb::new(",
    "Rgb::from_unit(",
    "Rgb::parse_hex(",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| panic!("{dir:?}: {err}"));
    for entry in entries {
        let path = entry.expect("entrada").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Literales `"#rgb"`, `"#rrggbb"` o `"#rrggbbaa"` en una línea.
fn hex_literal(line: &str) -> Option<&str> {
    let mut rest = line;
    while let Some(at) = rest.find("\"#") {
        let tail = &rest[at + 2..];
        let len = tail.chars().take_while(char::is_ascii_hexdigit).count();
        if matches!(len, 3 | 6 | 8) && tail[len..].starts_with('"') {
            return Some(&rest[at..at + len + 3]);
        }
        rest = &rest[at + 2..];
    }
    None
}

/// Problemas de un fichero (sin contar los comentarios de documentación ni los tests, donde los
/// colores se escriben para comprobar la paleta).
fn findings(path: &Path) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("{path:?}: {err}"));
    let mut out = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        if code.starts_with("#[cfg(test)]") {
            break;
        }
        for pattern in FORBIDDEN {
            if code.contains(pattern) {
                out.push(format!("{}:{}: {pattern}", path.display(), number + 1));
            }
        }
        if let Some(literal) = hex_literal(code) {
            out.push(format!("{}:{}: {literal}", path.display(), number + 1));
        }
    }
    out
}

#[test]
fn colors_only_live_in_the_theme() {
    let root = root();
    let ui = root.join("crates/ui/src");
    let app = root.join("apps/desktop/src/ui");
    assert!(app.is_dir(), "falta {app:?}");
    let mut files = Vec::new();
    sources(&ui, &mut files);
    sources(&app, &mut files);
    let theme = ui.join("theme.rs");
    assert!(files.contains(&theme), "el tema está en {theme:?}");
    let problems: Vec<String> = files
        .iter()
        .filter(|path| **path != theme)
        .flat_map(|path| findings(path))
        .collect();
    assert!(
        problems.is_empty(),
        "colores fijos fuera de crates/ui/src/theme.rs:\n{}",
        problems.join("\n")
    );
    assert!(files.len() > 5, "se revisaron {} ficheros", files.len());
}

#[test]
fn the_checker_finds_hand_written_colors() {
    let dir = std::env::temp_dir().join(format!("coroom-colores-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("directorio");
    let file = dir.join("mal.rs");
    fs::write(
        &file,
        "// Color::from_rgb en un comentario no cuenta\n\
         let a = Color::from_rgb8(1, 2, 3);\n\
         let b = \"#3584e4\";\n\
         let c = Color::TRANSPARENT;\n\
         #[cfg(test)]\n\
         let d = Color::WHITE;\n",
    )
    .expect("fichero");
    let found = findings(&file);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0].contains("Color::from_rgb"));
    assert!(found[1].contains("#3584e4"));
    assert_eq!(hex_literal("x = \"#fff\";"), Some("\"#fff\""));
    assert_eq!(hex_literal("#[derive(Debug)]"), None);
}
