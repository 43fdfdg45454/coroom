//! Test de capas (`docs/ARQUITECTURA.md` §5): lee el `Cargo.toml` de cada miembro del workspace y falla si una
//! crate depende de otra del workspace que su capa no permite.
//!
//! - `proto` y `crypto` no dependen de ninguna crate del workspace.
//! - `transport` solo de `proto` y `crypto`.
//! - `session` solo de `capture`, `codec`, `audio`, `input`, `transport`, `proto` y `crypto`.
//! - `ui` (sistema de diseño) no depende de ninguna crate del workspace.
//! - `apps/*` solo de `session`, `render`, `transport` y `ui`.
//!
//! Cuentan `[dependencies]`, `[build-dependencies]` y sus variantes `[target.'cfg(..)'.*]`; las
//! `[dev-dependencies]` quedan fuera porque no forman parte de la crate publicada.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use toml::Table;

/// Crates de `crates/` que deben existir.
const LIBRARY_CRATES: [&str; 11] = [
    "proto",
    "crypto",
    "transport",
    "capture",
    "codec",
    "audio",
    "input",
    "render",
    "session",
    "ui",
    "testkit",
];

/// Aplicaciones de `apps/` que deben existir.
const APPS: [&str; 2] = ["desktop", "server"];

struct Member {
    package: String,
    manifest: PathBuf,
    workspace_deps: BTreeSet<String>,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("raíz del workspace")
}

fn read_manifest(path: &Path) -> Table {
    let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("{path:?}: {err}"));
    text.parse::<Table>()
        .unwrap_or_else(|err| panic!("{path:?}: {err}"))
}

/// Nombres de paquete reales de las dependencias de una tabla (respeta `package = "..."`).
fn dependency_packages(table: Option<&toml::Value>) -> Vec<String> {
    let Some(table) = table.and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    table
        .iter()
        .map(|(key, spec)| {
            spec.get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key)
                .to_owned()
        })
        .collect()
}

fn normal_dependencies(manifest: &Table) -> Vec<String> {
    let mut deps = Vec::new();
    for section in ["dependencies", "build-dependencies"] {
        deps.extend(dependency_packages(manifest.get(section)));
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            for section in ["dependencies", "build-dependencies"] {
                deps.extend(dependency_packages(target.get(section)));
            }
        }
    }
    deps
}

/// Miembros del workspace indexados por su carpeta (`crates/proto`, `apps/server`, …).
fn members() -> BTreeMap<String, Member> {
    let root = workspace_root();
    let mut manifests = Vec::new();
    for group in ["crates", "apps"] {
        let dir = root.join(group);
        for entry in fs::read_dir(&dir).unwrap_or_else(|err| panic!("{dir:?}: {err}")) {
            let path = entry.expect("entrada de directorio").path();
            let manifest = path.join("Cargo.toml");
            if manifest.is_file() {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("nombre de carpeta UTF-8");
                manifests.push((format!("{group}/{name}"), manifest));
            }
        }
    }

    let parsed: Vec<(String, PathBuf, Table)> = manifests
        .into_iter()
        .map(|(dir, path)| {
            let table = read_manifest(&path);
            (dir, path, table)
        })
        .collect();
    let packages: BTreeMap<String, String> = parsed
        .iter()
        .map(|(dir, path, table)| {
            let package = table
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("{path:?}: falta package.name"));
            (package.to_owned(), dir.clone())
        })
        .collect();

    parsed
        .into_iter()
        .map(|(dir, manifest, table)| {
            let workspace_deps = normal_dependencies(&table)
                .into_iter()
                .filter_map(|dep| packages.get(&dep).cloned())
                .collect();
            let package = table
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            (
                dir,
                Member {
                    package,
                    manifest,
                    workspace_deps,
                },
            )
        })
        .collect()
}

fn assert_allowed(members: &BTreeMap<String, Member>, dir: &str, allowed: &[&str]) {
    let member = members
        .get(dir)
        .unwrap_or_else(|| panic!("falta el miembro {dir}"));
    let allowed: BTreeSet<String> = allowed
        .iter()
        .map(|name| format!("crates/{name}"))
        .collect();
    let forbidden: Vec<&String> = member.workspace_deps.difference(&allowed).collect();
    assert!(
        forbidden.is_empty(),
        "{} ({:?}) depende de {forbidden:?}, fuera de su capa (`docs/ARQUITECTURA.md` §5)",
        member.package,
        member.manifest
    );
}

#[test]
fn every_planned_crate_exists_with_its_package_name() {
    let members = members();
    for name in LIBRARY_CRATES {
        let member = members
            .get(&format!("crates/{name}"))
            .unwrap_or_else(|| panic!("falta crates/{name}"));
        assert_eq!(member.package, format!("coroom-{name}"));
    }
    assert_eq!(members["apps/desktop"].package, "coroom");
    assert_eq!(members["apps/server"].package, "coroom-server");
    assert_eq!(members.len(), LIBRARY_CRATES.len() + APPS.len());
}

#[test]
fn proto_and_crypto_do_not_depend_on_the_workspace() {
    let members = members();
    assert_allowed(&members, "crates/proto", &[]);
    assert_allowed(&members, "crates/crypto", &[]);
}

#[test]
fn the_design_system_does_not_depend_on_the_workspace() {
    let members = members();
    assert_allowed(&members, "crates/ui", &[]);
}

#[test]
fn transport_only_depends_on_proto_and_crypto() {
    let members = members();
    assert_allowed(&members, "crates/transport", &["proto", "crypto"]);
}

#[test]
fn session_only_orchestrates_lower_layers() {
    let members = members();
    assert_allowed(
        &members,
        "crates/session",
        &[
            "capture",
            "codec",
            "audio",
            "input",
            "transport",
            "proto",
            "crypto",
        ],
    );
}

#[test]
fn apps_only_depend_on_session_render_transport_and_ui() {
    let members = members();
    for app in APPS {
        assert_allowed(
            &members,
            &format!("apps/{app}"),
            &["session", "render", "transport", "ui"],
        );
    }
}

#[test]
fn the_checker_detects_a_violation() {
    let manifest: Table = r#"
        [package]
        name = "coroom-proto"

        [dependencies]
        renamed = { package = "coroom-crypto", workspace = true }

        [target.'cfg(windows)'.dependencies]
        coroom-transport = { path = "../transport" }

        [dev-dependencies]
        coroom-session = { path = "../session" }
    "#
    .parse()
    .expect("manifiesto de prueba");
    let deps = normal_dependencies(&manifest);
    assert!(deps.contains(&"coroom-crypto".to_owned()));
    assert!(deps.contains(&"coroom-transport".to_owned()));
    assert!(!deps.contains(&"coroom-session".to_owned()));
}
