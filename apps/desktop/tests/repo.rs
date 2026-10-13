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
