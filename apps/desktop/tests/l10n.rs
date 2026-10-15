//! Catálogos de textos: ningún texto visible de la interfaz queda fuera de
//! `locales/es/*.ftl`, y código y catálogos se corresponden.
//!
//! - **Heurística de textos sueltos**: ningún literal con acentos, eñes o signos de apertura
//!   (`áéíóúüñ¿¡`) en `src/ui` fuera de los tests, de los registros (`debug!`, `info!`, `warn!`,
//!   `error!`, `trace!`: el registro no es interfaz) y de las comparaciones con errores de otras
//!   crates (`.contains("…")`). `src/ui/shots.rs` y el `mod shots` del final de cada pantalla
//!   quedan fuera: son los datos de ejemplo de las capturas (nombres, salas), no textos de la
//!   interfaz.
//! - Todo `t!("id")` del código existe en el catálogo, con las mismas variables.
//! - Todo mensaje del catálogo se usa en algún sitio.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use coroom_desktop::l10n;

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("directorio") {
        let path = entry.expect("entrada").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Fuente con finales de línea `\n` también cuando el checkout es CRLF (Windows).
fn read_source(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .expect("fuente")
        .replace("\r\n", "\n")
}

/// El código sin el módulo de tests ni el de las escenas de las capturas (`mod shots`, al final
/// de cada pantalla, antes de los tests: son datos de ejemplo, no textos de la interfaz).
fn without_tests(source: &str) -> &str {
    let mut end = source.len();
    for marker in [
        "\n#[cfg(test)]\nmod ",
        "\n#[cfg(test)]\npub(crate) mod ",
        "\npub(crate) mod shots {",
    ] {
        if let Some(at) = source.find(marker) {
            end = end.min(at);
        }
    }
    &source[..end]
}

/// Literales `"…"` con la línea en que empiezan y lo que hay antes en esa línea y en la anterior.
fn literals(code: &str) -> Vec<(usize, String, String)> {
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\'' => {
                // Carácter (`'a'`, `'\n'`, `'«'`) o tiempo de vida.
                let rest = &code[i + 1..];
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some('\\'), _) => {
                        let close = rest[1..].find('\'').map_or(1, |at| at + 2);
                        i += 1 + close;
                    }
                    (Some(c), Some('\'')) => i += 2 + c.len_utf8(),
                    _ => i += 1,
                }
            }
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let line_start = code[..start].rfind('\n').map_or(0, |at| at + 1);
                let context_start = code[..line_start.saturating_sub(1)]
                    .rfind('\n')
                    .map_or(0, |at| at + 1);
                let line = code[..start].matches('\n').count() + 1;
                out.push((
                    line,
                    code[start..i.min(bytes.len())].to_owned(),
                    code[context_start..start].to_owned(),
                ));
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

fn is_spanish(text: &str) -> bool {
    text.chars().any(|c| "áéíóúüñÁÉÍÓÚÜÑ¿¡".contains(c))
}

#[test]
fn no_visible_text_is_left_outside_the_catalog() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
    let mut files = Vec::new();
    sources(&dir, &mut files);
    let mut loose = Vec::new();
    for path in files {
        if path.file_name().is_some_and(|name| name == "shots.rs") {
            continue;
        }
        let source = read_source(&path);
        for (line, text, context) in literals(without_tests(&source)) {
            if !is_spanish(&text) {
                continue;
            }
            let logging = ["debug!(", "info!(", "warn!(", "error!(", "trace!("]
                .iter()
                .any(|m| context.contains(m));
            if logging
                || context
                    .trim_end_matches('"')
                    .trim_end()
                    .ends_with(".contains(")
            {
                continue;
            }
            loose.push(format!("{}:{line}: «{text}»", path.display()));
        }
    }
    assert!(
        loose.is_empty(),
        "textos de la interfaz fuera de locales/es/*.ftl:\n{}",
        loose.join("\n")
    );
}

/// Llamadas `t!("id", a = …, b = …)` del código: identificador → variables.
fn uses() -> BTreeMap<String, BTreeSet<String>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&dir, &mut files);
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for path in files {
        let source = read_source(&path);
        let mut rest = source.as_str();
        while let Some(at) = rest.find("t!(") {
            // `format!("…")` también acaba en `t!(`: hace falta un límite de palabra delante.
            let boundary = rest[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            // `rustfmt` parte las llamadas largas: el identificador puede ir en la línea siguiente.
            let after = &rest[at + 3..];
            let call = after.trim_start();
            if !boundary || !call.starts_with('"') {
                rest = after;
                continue;
            }
            let call = &call[1..];
            let Some(close) = call.find('"') else { break };
            let id = call[..close].to_owned();
            // Variables: `nombre =` en el primer nivel de la llamada.
            let mut names = BTreeSet::new();
            let mut depth = 0i32;
            let args = &call[close + 1..];
            let mut end = args.len();
            for (index, c) in args.char_indices() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' if depth == 0 => {
                        end = index;
                        break;
                    }
                    ')' | ']' | '}' => depth -= 1,
                    _ => {}
                }
            }
            let mut level = 0i32;
            let mut token = String::new();
            for c in args[..end].chars() {
                match c {
                    '(' | '[' | '{' => level += 1,
                    ')' | ']' | '}' => level -= 1,
                    ',' if level == 0 => token.clear(),
                    '=' if level == 0 => {
                        let name = token.trim().to_owned();
                        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                        {
                            names.insert(name);
                        }
                        token.clear();
                    }
                    _ => token.push(c),
                }
            }
            if id != "id" {
                out.entry(id).or_default().extend(names);
            }
            rest = &call[close..];
        }
    }
    out
}

/// Mensajes de los catálogos del español: identificador → variables.
fn catalog() -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for source in l10n::CATALOGS[0].1 {
        catalog_of(source, &mut out);
    }
    out
}

fn catalog_of(source: &str, out: &mut BTreeMap<String, BTreeSet<String>>) {
    let ids = l10n::message_ids(source);
    let before = out.len();
    for line in source.lines() {
        let Some((id, value)) = line.split_once(" = ") else {
            continue;
        };
        if !ids.iter().any(|known| known == id) {
            continue;
        }
        let mut variables = BTreeSet::new();
        let mut rest = value;
        while let Some(at) = rest.find("{ $") {
            let name: String = rest[at + 3..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            variables.insert(name);
            rest = &rest[at + 3..];
        }
        assert!(
            out.insert(id.to_owned(), variables).is_none(),
            "«{id}» repetido entre catálogos"
        );
    }
    assert_eq!(
        out.len() - before,
        ids.len(),
        "un mensaje por línea en el catálogo"
    );
}

#[test]
fn code_and_catalog_match() {
    let uses = uses();
    let catalog = catalog();
    assert!(
        uses.len() > 300,
        "{} identificadores en el código",
        uses.len()
    );
    let mut problems = Vec::new();
    for (id, names) in &uses {
        match catalog.get(id) {
            None => problems.push(format!("«{id}» no está en el catálogo")),
            Some(variables) if variables != names => problems.push(format!(
                "«{id}»: el código pasa {names:?} y el catálogo usa {variables:?}"
            )),
            Some(_) => {}
        }
    }
    for id in catalog.keys() {
        if !uses.contains_key(id) {
            problems.push(format!("«{id}» del catálogo no se usa"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn texts_render_in_spanish() {
    assert_eq!(coroom_desktop::t!("app-inicio"), "Inicio");
    assert_eq!(
        coroom_desktop::t!("home-invite-title", name = "Bea"),
        "Bea te invita a su sala"
    );
    // Mismo puntero: los textos sin variables se formatean una vez.
    assert!(std::ptr::eq(
        coroom_desktop::t!("app-inicio"),
        coroom_desktop::t!("app-inicio")
    ));
}
