//! Comprobaciones del repositorio que no son de la aplicación pero rompen la CI si se olvidan.
//! Solo en Unix: `tools/check-locks.sh` es de bash (la CI lo pasa en Linux).
#![cfg(unix)]

use std::path::Path;
use std::process::Command;

/// `tools/check-locks.sh`: los `Cargo.lock` del workspace y de `fuzz/` (workspace aparte, que la
/// CI compila con `--locked`) y `cargo-sources.json` del Flatpak, al día con las dependencias. Así
/// `cargo test` falla antes de subir un cambio de dependencias que deja uno viejo.
#[test]
fn locks_are_up_to_date() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(root.join("tools/check-locks.sh"))
        .current_dir(&root)
        .output()
        .expect("tools/check-locks.sh");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Regla de privacidad del CLAUDE.md: el repositorio es público y no lleva datos del equipo de
/// nadie. Un modelo concreto de GPU o CPU («RTX 5090», «RX 7800», «Ryzen 7 9800X», «Core i9»)
/// identifica la máquina del propietario; las familias («RTX 50», «GeForce», «Radeon») sirven
/// para documentar drivers y son genéricas. Se miran los ficheros de texto que git sigue.
#[test]
fn no_file_names_a_specific_gpu_or_cpu_model() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(&root)
        .output()
        .expect("git ls-files");
    assert!(output.status.success(), "git ls-files");
    let mut offenders = Vec::new();
    for path in output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path).into_owned();
        let text_file = [
            ".rs", ".md", ".toml", ".ftl", ".yml", ".yaml", ".sh", ".ps1", ".txt", ".json",
        ]
        .iter()
        .any(|ext| path.ends_with(ext));
        if !text_file {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&path)) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            if let Some(model) = hardware_model(line) {
                offenders.push(format!("{path}:{}: «{model}»", number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "modelos concretos de hardware en el repositorio (regla de privacidad):\n{}",
        offenders.join("\n")
    );
}

/// El primer modelo concreto de GPU o CPU que nombra `line`: una marca de modelo seguida de un
/// número de tres o más cifras («RTX 5090», «RX 7800», «Ryzen 9 9950X» por «Ryzen 9», «Core i7»
/// por «i7» con sufijo numérico de modelo, «Arc A770»).
fn hardware_model(line: &str) -> Option<String> {
    const BRANDS: [&str; 7] = ["RTX", "GTX", "RX", "Arc A", "Arc B", "Ryzen", "Core i"];
    let words: Vec<&str> = line.split_whitespace().collect();
    for (index, word) in words.iter().enumerate() {
        for brand in BRANDS {
            let rest = if brand.contains(' ') {
                let (first, second) = brand.split_once(' ')?;
                if *word != first {
                    continue;
                }
                let Some(next) = words.get(index + 1) else {
                    continue;
                };
                next.strip_prefix(second)
            } else {
                word.strip_prefix(brand).and_then(|tail| {
                    if tail.is_empty() {
                        words.get(index + 1).copied()
                    } else {
                        Some(tail)
                    }
                })
            };
            let Some(rest) = rest else {
                continue;
            };
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let model_number = if brand == "Ryzen" || brand == "Core i" {
                // «Ryzen 7» o «Core i7» a secas son gamas; con cuatro cifras («Ryzen 7 9800X») es
                // un modelo.
                let after: Vec<&str> = words
                    .get(index + 2..)
                    .map(|w| w.to_vec())
                    .unwrap_or_default();
                digits.len() >= 3
                    || after
                        .first()
                        .is_some_and(|w| w.chars().take_while(char::is_ascii_digit).count() >= 3)
            } else {
                digits.len() >= 3
            };
            if model_number {
                return Some(words[index..(index + 3).min(words.len())].join(" "));
            }
        }
    }
    None
}

#[test]
fn hardware_models_are_recognised_but_families_are_not() {
    // Los ejemplos van partidos para que este fichero tampoco nombre ningún modelo.
    for model in [
        concat!("en una RTX ", "5090 con Linux"),
        concat!("GeForce RTX", "5090 Ti"),
        concat!("una Radeon RX ", "7800 XT"),
        concat!("un Ryzen 7 ", "9800X"),
        concat!("un Core i7 ", "14700K"),
        concat!("Intel Arc A", "770"),
    ] {
        assert!(hardware_model(model).is_some(), "{model}");
    }
    for family in [
        "las NVIDIA RTX 50 (Blackwell)",
        "GeForce de sobremesa reciente, Radeon RDNA",
        "un Ryzen 7 cualquiera, un Core i5",
        "GPU de ejemplo / driver 1.2.3",
    ] {
        assert!(hardware_model(family).is_none(), "{family}");
    }
}
