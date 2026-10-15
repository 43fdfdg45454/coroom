//! Sondeo de Vulkan Video en un **proceso hijo**: el mismo ejecutable, relanzado con la variable
//! de entorno [`ENV_CHILD`], abre el codificador o decodificador y dice `ok` o el error; si el
//! proceso muere (SIGSEGV de un cargador Vulkan viejo con un driver nuevo, o un driver roto), el
//! padre lo cuenta como «no disponible» con el motivo y sigue con las demás combinaciones.
//!
//! Solo se usa cuando el binario lo ha activado llamando a [`run_if_requested`] al arrancar
//! (`coroom` lo hace antes de leer la línea de órdenes): un binario de tests, por ejemplo, no
//! entiende la variable y volvería a ejecutar sus tests, así que sin activarlo todo va en el
//! proceso, como antes.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use crate::{Backend, Codec, DecoderBackend};

/// Variable de entorno con la orden del hijo: `enc:<códec>:<backend>` o `dec:<códec>:<backend>`
/// (los ids cortos de [`Codec::id`], [`Backend::id`] y [`DecoderBackend::id`]).
pub const ENV_CHILD: &str = "COROOM_PROBE_CHILD";

/// Con `COROOM_PROBE_IN_PROCESS=1` el sondeo no relanza nada (para depurar con el proceso en un
/// depurador).
pub const ENV_IN_PROCESS: &str = "COROOM_PROBE_IN_PROCESS";

/// Tiempo máximo del hijo: abrir el dispositivo y el codificador y sacar un paquete.
const CHILD_TIMEOUT: Duration = Duration::from_secs(8);

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Si este proceso es un hijo de sondeo ([`ENV_CHILD`] definida), hace el sondeo pedido, escribe
/// el resultado en la salida estándar y devuelve `true`: el binario debe terminar sin hacer nada
/// más. Si no, activa el sondeo en proceso hijo para el resto de la ejecución y devuelve `false`.
pub fn run_if_requested() -> bool {
    ENABLED.store(true, Ordering::Release);
    let Ok(order) = std::env::var(ENV_CHILD) else {
        return false;
    };
    // Sin ruido de FFmpeg en la salida que lee el padre.
    super::init_logging();
    super::set_log_level(rsmpeg::ffi::AV_LOG_QUIET);
    let result = run_order(&order);
    match result {
        Ok(()) => println!("ok"),
        Err(reason) => println!("error: {}", reason.replace('\n', " ")),
    }
    true
}

/// Ejecuta la orden de [`ENV_CHILD`] en este proceso.
fn run_order(order: &str) -> Result<(), String> {
    let mut parts = order.split(':');
    let (kind, codec, backend) = (parts.next(), parts.next(), parts.next());
    let codec = codec
        .and_then(Codec::from_id)
        .ok_or_else(|| format!("orden de sondeo no válida: {order}"))?;
    match kind {
        Some("enc") => {
            let backend = backend
                .and_then(Backend::from_id)
                .ok_or_else(|| format!("backend no válido en {order}"))?;
            super::probe::open_and_encode(codec, backend)
        }
        Some("dec") => {
            let backend = backend
                .and_then(DecoderBackend::from_id)
                .ok_or_else(|| format!("backend no válido en {order}"))?;
            super::FfmpegDecoder::new(codec, backend)
                .map(|_| ())
                .map_err(|err| err.to_string())
        }
        _ => Err(format!("orden de sondeo no válida: {order}")),
    }
}

/// Si el sondeo en proceso hijo está activo ([`run_if_requested`] y sin [`ENV_IN_PROCESS`]).
fn isolation_enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
        && std::env::var(ENV_CHILD).is_err()
        && !std::env::var(ENV_IN_PROCESS).is_ok_and(|v| v == "1")
}

/// Abre el codificador `codec`/`backend` en un proceso hijo (ver el módulo). `Ok` si abrió y sacó
/// un paquete, o si el sondeo aislado no está activo; `Err` con el motivo si falló o murió.
pub(crate) fn isolated_encoder_open(codec: Codec, backend: Backend) -> Result<(), String> {
    if !isolation_enabled() {
        return Ok(());
    }
    run_child(&format!("enc:{}:{}", codec.id(), backend.id()))
}

/// Como [`isolated_encoder_open`], con un decodificador.
pub(crate) fn isolated_decoder_open(codec: Codec, backend: DecoderBackend) -> Result<(), String> {
    if !isolation_enabled() {
        return Ok(());
    }
    run_child(&format!("dec:{}:{}", codec.id(), backend.id()))
}

/// Veredictos ya obtenidos en este proceso, por orden (`enc:av1:vulkan`): cada combinación se
/// relanza una sola vez aunque el sondeo la pregunte varias veces (decodificador y `hwaccel`).
fn verdicts() -> &'static Mutex<HashMap<String, Result<(), String>>> {
    static VERDICTS: OnceLock<Mutex<HashMap<String, Result<(), String>>>> = OnceLock::new();
    VERDICTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// [`spawn_child`] con el veredicto guardado por orden.
fn run_child(order: &str) -> Result<(), String> {
    if let Some(verdict) = verdicts()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(order)
    {
        return verdict.clone();
    }
    let verdict = spawn_child(order);
    verdicts()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(order.to_owned(), verdict.clone());
    verdict
}

/// Relanza este ejecutable con la orden y lee su veredicto.
fn spawn_child(order: &str) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|err| format!("no se sabe qué ejecutable relanzar para el sondeo: {err}"))?;
    let mut child = Command::new(exe)
        .env(ENV_CHILD, order)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("no se pudo lanzar el proceso de sondeo: {err}"))?;
    let mut stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_string(&mut text);
        }
        let _ = tx.send(text);
    });
    let output = match rx.recv_timeout(CHILD_TIMEOUT) {
        Ok(text) => text,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "el sondeo en un proceso aparte no respondió en {} s",
                CHILD_TIMEOUT.as_secs()
            ));
        }
    };
    let status = child
        .wait()
        .map_err(|err| format!("el proceso de sondeo no terminó bien: {err}"))?;
    let verdict = output.lines().last().unwrap_or("").trim();
    if status.success() && verdict == "ok" {
        return Ok(());
    }
    if let Some(reason) = verdict.strip_prefix("error: ") {
        return Err(reason.to_owned());
    }
    Err(format!(
        "el sondeo murió de golpe ({}); suele ser un cargador Vulkan (libvulkan1) demasiado viejo \
         para el driver o para FFmpeg: actualízalo o usa otro backend",
        describe_status(&status)
    ))
}

/// `status` legible: la señal en Unix (`SIGSEGV`), el código si no.
fn describe_status(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let name = match signal {
                libc::SIGSEGV => "SIGSEGV",
                libc::SIGABRT => "SIGABRT",
                libc::SIGBUS => "SIGBUS",
                libc::SIGILL => "SIGILL",
                libc::SIGFPE => "SIGFPE",
                _ => "señal",
            };
            return format!("{name} {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("código {code}"),
        None => "sin código".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_are_parsed_and_isolation_is_off_until_enabled() {
        assert!(run_order("enc:h264").is_err());
        assert!(run_order("foo:h264:vulkan").is_err());
        assert!(run_order("enc:h264:inexistente").is_err());
        // Sin activar, el sondeo va en proceso y la comprobación aislada no bloquea nada.
        assert_eq!(
            isolated_encoder_open(Codec::Av1, Backend::VulkanVideo),
            Ok(())
        );
        assert_eq!(
            isolated_decoder_open(Codec::Av1, DecoderBackend::Vulkan),
            Ok(())
        );
    }
}
