//! Descarga de un paquete de una publicación, con pausa, reanudación y su SHA-256.
//!
//! El paquete se escribe en `<dir>/<nombre>.<sha256 abreviado>.part` ([`part_name`]) y, comprobado
//! el SHA-256 contra el `digest` que publica GitHub, se renombra a `<dir>/<nombre>`. Pausar
//! ([`DownloadControl::pause`]) corta la conexión y deja el `.part`: la siguiente descarga del
//! mismo paquete pide el resto con `Range` y, si el servidor no lo admite (responde 200), vuelve a
//! empezar. Como el `.part` lleva el SHA-256 en el nombre, un paquete que se volvió a subir con
//! otro contenido no se mezcla con lo bajado antes; y el tamaño total que dice el servidor
//! (`Content-Range` o `Content-Length`) tiene que ser el que publica GitHub.
//!
//! La carpeta es solo de las descargas: al empezar una se borran las de otras versiones
//! (`coroom-*`).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use coroom_transport::TransportError;
use coroom_transport::download;
use sha2::{Digest, Sha256};

use super::Asset;

/// Cuántas veces se vuelve a pedir el paquete si el servidor no entiende el rango pedido.
const RANGE_RETRIES: usize = 1;

/// Pausa de una descarga en curso, desde otro hilo.
#[derive(Debug, Default)]
pub struct DownloadControl {
    paused: AtomicBool,
    wake: tokio::sync::Notify,
}

impl DownloadControl {
    /// Un control sin pausar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pausa la descarga: corta la conexión y [`download`] vuelve con [`Downloaded::Paused`].
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }

    /// Si se pidió la pausa.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Termina cuando se pide la pausa (enseguida si ya se pidió): para quien descarga por otro
    /// camino (el portal de Flatpak).
    pub async fn wait_paused(&self) {
        loop {
            let woken = self.wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            if self.is_paused() {
                return;
            }
            woken.await;
        }
    }
}

/// Cómo terminó [`download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Downloaded {
    /// Entero y comprobado, en esta ruta.
    Done(PathBuf),
    /// En pausa, con `done` de `total` bytes ya en el disco.
    Paused {
        /// Bytes descargados.
        done: u64,
        /// Tamaño del paquete.
        total: u64,
    },
}

/// SHA-256 de un fichero, leído por trozos.
pub fn sha256_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().into())
}

/// Comprueba que el SHA-256 de `path` es `expected`; si no, por qué.
pub fn verify_sha256(path: &Path, expected: &[u8; 32], name: &str) -> Result<(), String> {
    let actual =
        sha256_file(path).map_err(|err| format!("no se pudo leer {}: {err}", path.display()))?;
    if &actual == expected {
        Ok(())
    } else {
        Err(format!(
            "el SHA-256 de {name} no coincide con el que publica GitHub; se descartó la descarga"
        ))
    }
}

/// Nombre del fichero a medias de `asset`: `<nombre>.<16 primeras cifras del SHA-256>.part`.
pub fn part_name(asset: &Asset, sha256: &[u8; 32]) -> String {
    let short: String = sha256[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{}.{short}.part", asset.name)
}

/// Borra de `dir` las descargas (`coroom-*`) que no sean `keep`.
fn remove_others(dir: &Path, keep: &[&Path]) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = entry.file_name().to_string_lossy().starts_with("coroom-");
        if ours && path.is_file() && !keep.contains(&path.as_path()) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// El texto de un error de la red al descargar.
fn network(err: &TransportError) -> String {
    match err {
        TransportError::Timeout(_) => "GitHub no respondió a tiempo".to_owned(),
        err => format!("no se pudo descargar: {err}"),
    }
}

/// Descarga `asset` en `dir` (bloquea): reanuda un `.part` anterior, avisa con
/// `progress(hechos, total)` cada vez que avanza una milésima y comprueba el SHA-256 al terminar.
/// Con un paquete ya descargado y comprobado vuelve enseguida. Sin el SHA-256 que publica GitHub no
/// se descarga: no habría con qué comprobarlo. Antes de empezar borra de `dir` las descargas de
/// otras versiones.
pub fn download(
    asset: &Asset,
    dir: &Path,
    user_agent: &str,
    control: &DownloadControl,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Downloaded, String> {
    let expected = asset.sha256.ok_or_else(|| {
        format!(
            "GitHub no publica el SHA-256 de {}; sin él no se instala",
            asset.name
        )
    })?;
    if asset.name.contains(['/', '\\']) || asset.name.starts_with('.') {
        return Err(format!("nombre de paquete no válido: {}", asset.name));
    }
    fs::create_dir_all(dir).map_err(|err| format!("no se pudo crear {}: {err}", dir.display()))?;
    let done_path = dir.join(&asset.name);
    let part_path = dir.join(part_name(asset, &expected));
    remove_others(dir, &[&done_path, &part_path]);
    if done_path.exists() {
        if verify_sha256(&done_path, &expected, &asset.name).is_ok() {
            progress(asset.size, asset.size);
            return Ok(Downloaded::Done(done_path));
        }
        let _ = fs::remove_file(&done_path);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("no se pudo preparar la red: {err}"))?;
    let total = asset.size;
    let headers = [("user-agent", user_agent.to_owned())];
    let mut retries = RANGE_RETRIES;
    loop {
        let mut from = fs::metadata(&part_path).map_or(0, |meta| meta.len());
        if from > total {
            from = 0;
        }
        if from < total || total == 0 {
            match fetch(
                &runtime, asset, &part_path, from, &headers, control, progress,
            )? {
                Fetch::Finished => {}
                Fetch::Paused(done) => return Ok(Downloaded::Paused { done, total }),
                Fetch::RangeRefused if retries > 0 => {
                    retries -= 1;
                    let _ = fs::remove_file(&part_path);
                    continue;
                }
                Fetch::RangeRefused => {
                    let _ = fs::remove_file(&part_path);
                    return Err(format!(
                        "el servidor de GitHub no entregó {} entero",
                        asset.name
                    ));
                }
            }
        }
        let size = fs::metadata(&part_path).map_or(0, |meta| meta.len());
        if size != total {
            let _ = fs::remove_file(&part_path);
            return Err(format!(
                "{} llegó con {size} bytes y GitHub dice {total}; se descartó",
                asset.name
            ));
        }
        if let Err(err) = verify_sha256(&part_path, &expected, &asset.name) {
            let _ = fs::remove_file(&part_path);
            return Err(err);
        }
        fs::rename(&part_path, &done_path)
            .map_err(|err| format!("no se pudo guardar {}: {err}", done_path.display()))?;
        return Ok(Downloaded::Done(done_path));
    }
}

enum Fetch {
    Finished,
    Paused(u64),
    /// El servidor no sirve el rango pedido (416): se empieza de cero.
    RangeRefused,
}

fn fetch(
    runtime: &tokio::runtime::Runtime,
    asset: &Asset,
    part_path: &Path,
    from: u64,
    headers: &[(&'static str, String)],
    control: &DownloadControl,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Fetch, String> {
    let total = asset.size;
    runtime.block_on(async {
        let woken = control.wake.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        if control.is_paused() {
            return Ok(Fetch::Paused(from));
        }
        let mut response = tokio::select! {
            response = download::get(&asset.url, headers, from) => {
                response.map_err(|err| network(&err))?
            }
            () = &mut woken => return Ok(Fetch::Paused(from)),
        };
        // El tamaño que dice el servidor tiene que ser el que publica GitHub.
        let announced = match response.status.as_u16() {
            206 => response.range.and_then(|(_, total)| total),
            _ => response.content_length,
        };
        if total > 0 && announced.is_some_and(|announced| announced != total) {
            return Err(format!(
                "{} no tiene el tamaño que publica GitHub; se reintentará más tarde",
                asset.name
            ));
        }
        let mut done = match response.status.as_u16() {
            206 if response.range.is_some_and(|(start, _)| start == from) => from,
            200 => 0,
            206 | 416 => return Ok(Fetch::RangeRefused),
            status => return Err(format!("GitHub respondió {status} al descargar")),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(done > 0)
            .truncate(done == 0)
            .open(part_path)
            .map_err(|err| format!("no se pudo escribir {}: {err}", part_path.display()))?;
        let step = (total / 1000).max(64 * 1024);
        let mut reported = done;
        progress(done, total);
        loop {
            // La notificación se pide antes de mirar la marca: una pausa entre las dos no se pierde.
            let woken = control.wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            if control.is_paused() {
                break;
            }
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk.map_err(|err| network(&err))?,
                () = &mut woken => continue,
            };
            let Some(chunk) = chunk else {
                file.flush()
                    .map_err(|err| format!("no se pudo escribir la descarga: {err}"))?;
                progress(done, total);
                return Ok(Fetch::Finished);
            };
            if done + chunk.len() as u64 > total {
                return Err(format!(
                    "{} llegó más grande de lo que dice GitHub",
                    asset.name
                ));
            }
            file.write_all(&chunk)
                .map_err(|err| format!("no se pudo escribir la descarga: {err}"))?;
            done += chunk.len() as u64;
            if done - reported >= step {
                reported = done;
                progress(done, total);
            }
        }
        file.flush()
            .map_err(|err| format!("no se pudo escribir la descarga: {err}"))?;
        progress(done, total);
        Ok(Fetch::Paused(done))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(body: &[u8], sha: Option<[u8; 32]>) -> Asset {
        Asset {
            name: "coroom-2.1.0-windows-x64.zip".to_owned(),
            // No se llega a pedir: el paquete ya está en el disco.
            url: "https://github.com/43fdfdg45454/coroom/releases/download/v2.1.0/x.zip".to_owned(),
            size: body.len() as u64,
            sha256: sha,
        }
    }

    #[test]
    fn sha256_of_a_file_is_checked() {
        let dir = tempfile::tempdir().expect("directorio");
        let path = dir.path().join("x");
        fs::write(&path, b"abc").expect("escribir");
        // SHA-256("abc"), de FIPS 180-2.
        let abc = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(sha256_file(&path).expect("hash"), abc);
        assert!(verify_sha256(&path, &abc, "x").is_ok());
        assert!(
            verify_sha256(&path, &[0; 32], "x")
                .expect_err("no coincide")
                .contains("SHA-256")
        );
    }

    #[test]
    fn a_verified_package_on_disk_is_not_downloaded_again() {
        let dir = tempfile::tempdir().expect("directorio");
        let body = b"paquete de ejemplo".to_vec();
        let sha: [u8; 32] = Sha256::digest(&body).into();
        let asset = asset(&body, Some(sha));
        fs::write(dir.path().join(&asset.name), &body).expect("escribir");
        let mut seen = Vec::new();
        let control = DownloadControl::new();
        let result = download(
            &asset,
            dir.path(),
            "coroom/prueba",
            &control,
            &mut |d, t| {
                seen.push((d, t));
            },
        );
        assert_eq!(result, Ok(Downloaded::Done(dir.path().join(&asset.name))));
        assert_eq!(seen, [(body.len() as u64, body.len() as u64)]);
    }

    #[test]
    fn a_paused_control_stops_before_touching_the_network() {
        let dir = tempfile::tempdir().expect("directorio");
        let body = vec![7u8; 1000];
        let sha: [u8; 32] = Sha256::digest(&body).into();
        let asset = asset(&body, Some(sha));
        fs::write(dir.path().join(part_name(&asset, &sha)), &body[..300]).expect("parte");
        let control = DownloadControl::new();
        control.pause();
        let result = download(
            &asset,
            dir.path(),
            "coroom/prueba",
            &control,
            &mut |_, _| {},
        );
        assert_eq!(
            result,
            Ok(Downloaded::Paused {
                done: 300,
                total: 1000
            })
        );
    }

    #[test]
    fn other_versions_are_removed_and_parts_carry_the_digest() {
        let dir = tempfile::tempdir().expect("directorio");
        let body = b"paquete de ejemplo".to_vec();
        let sha: [u8; 32] = Sha256::digest(&body).into();
        let asset = asset(&body, Some(sha));
        assert!(part_name(&asset, &sha).starts_with("coroom-2.1.0-windows-x64.zip."));
        assert_ne!(part_name(&asset, &sha), part_name(&asset, &[0; 32]));
        let older = dir.path().join("coroom-2.0.9-windows-x64.zip");
        let older_part = dir
            .path()
            .join("coroom-2.0.9-windows-x64.zip.0011223344556677.part");
        let note = dir.path().join("fallo.txt");
        for path in [&older, &older_part, &note] {
            fs::write(path, b"x").expect("escribir");
        }
        fs::write(dir.path().join(&asset.name), &body).expect("escribir");
        let control = DownloadControl::new();
        assert_eq!(
            download(&asset, dir.path(), "ua", &control, &mut |_, _| {}),
            Ok(Downloaded::Done(dir.path().join(&asset.name)))
        );
        assert!(
            !older.exists() && !older_part.exists(),
            "las otras versiones se van"
        );
        assert!(note.exists(), "lo que no es una descarga se queda");
    }

    #[test]
    fn a_complete_part_is_verified_and_a_wrong_one_discarded() {
        let dir = tempfile::tempdir().expect("directorio");
        let body = vec![9u8; 2048];
        let sha: [u8; 32] = Sha256::digest(&body).into();
        let good = asset(&body, Some(sha));
        let part = dir.path().join(part_name(&good, &sha));
        fs::write(&part, &body).expect("parte");
        let control = DownloadControl::new();
        assert_eq!(
            download(&good, dir.path(), "ua", &control, &mut |_, _| {}),
            Ok(Downloaded::Done(dir.path().join(&good.name)))
        );
        assert!(!part.exists());

        let bad = asset(&body, Some([1; 32]));
        let _ = fs::remove_file(dir.path().join(&bad.name));
        let part = dir.path().join(part_name(&bad, &[1; 32]));
        fs::write(&part, &body).expect("parte");
        let err =
            download(&bad, dir.path(), "ua", &control, &mut |_, _| {}).expect_err("no coincide");
        assert!(err.contains("SHA-256"), "{err}");
        assert!(!part.exists(), "lo que no coincide se borra");

        let unsigned = asset(&body, None);
        assert!(
            download(&unsigned, dir.path(), "ua", &control, &mut |_, _| {})
                .expect_err("sin digest")
                .contains("SHA-256")
        );
    }
}
