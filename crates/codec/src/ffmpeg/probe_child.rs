//! Sondeo de Vulkan Video en un **proceso hijo**: el mismo ejecutable, relanzado con la variable
//! de entorno [`ENV_CHILD`], abre el codificador o decodificador y dice `ok` o el error; si el
//! proceso muere (SIGSEGV de un cargador Vulkan viejo con un driver nuevo, o un driver roto), el
//! padre lo cuenta como «no disponible» con el motivo y sigue con las demás combinaciones.
//!
//! El sondeo **entero** ([`isolated_probe`]) también va en un hijo, con prioridad baja: abrir
//! dispositivos (CUDA, VA-API, Vulkan) y codificar los fotogramas de la comprobación dentro del
//! proceso de la aplicación traba su interfaz (dibujos de 65 a 210 ms en el primer arranque, sin
//! la caché de `codecs.json`); en otro proceso, la ventana no comparte con él ni el driver ni la
//! CPU con la misma prioridad. Ese hijo sigue relanzando cada apertura de Vulkan Video en un
//! nieto, así que un SIGSEGV de Vulkan solo descarta esa combinación, como antes.
//!
//! Solo se usa cuando el binario lo ha activado llamando a [`run_if_requested`] al arrancar
//! (`coroom` lo hace antes de leer la línea de órdenes): un binario de tests, por ejemplo, no
//! entiende la variable y volvería a ejecutar sus tests, así que sin activarlo todo va en el
//! proceso, como antes.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::{Backend, Capabilities, Codec, DecoderBackend};

/// Variable de entorno con la orden del hijo: `enc:<códec>:<backend>` o `dec:<códec>:<backend>`
/// (los ids cortos de [`Codec::id`], [`Backend::id`] y [`DecoderBackend::id`]), o [`PROBE_ALL`].
pub const ENV_CHILD: &str = "COROOM_PROBE_CHILD";

/// Orden del hijo que hace el sondeo entero ([`isolated_probe`]).
const PROBE_ALL: &str = "caps";

/// Prefijo de la línea en la que el hijo de [`PROBE_ALL`] escribe las capacidades.
const CAPS_LINE: &str = "caps ";

/// Con `COROOM_PROBE_IN_PROCESS=1` el sondeo no relanza nada (para depurar con el proceso en un
/// depurador).
pub const ENV_IN_PROCESS: &str = "COROOM_PROBE_IN_PROCESS";

/// Tiempo máximo del hijo: abrir el dispositivo y el codificador y sacar un paquete.
pub(super) const CHILD_TIMEOUT: Duration = Duration::from_secs(8);

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Si este proceso es un hijo de sondeo ([`ENV_CHILD`] definida), hace el sondeo pedido, escribe
/// el resultado en la salida estándar y devuelve `true`: el binario debe terminar sin hacer nada
/// más. Si no, activa el sondeo en proceso hijo para el resto de la ejecución y devuelve `false`.
pub fn run_if_requested() -> bool {
    ENABLED.store(true, Ordering::Release);
    let Ok(order) = std::env::var(ENV_CHILD) else {
        return false;
    };
    if order == PROBE_ALL {
        lower_priority();
        let caps = Capabilities::probe_in_process();
        println!("{CAPS_LINE}{}", crate::caps::to_wire(&caps));
        return true;
    }
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

/// El hijo del sondeo entero cede la CPU a la aplicación: es trabajo de fondo, y con núcleos
/// libres corre igual de rápido. En Linux, `nice` 10 (el incremento por omisión de `nice(1)`,
/// que heredan los nietos); en Windows, la clase «por debajo de lo normal» (que también heredan).
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: setpriority sobre el propio proceso (who = 0) no toca memoria; si falla, el sondeo
    // sigue con la prioridad de siempre.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    #[cfg(target_os = "windows")]
    // SAFETY: GetCurrentProcess devuelve un pseudo-handle que no hay que cerrar; si
    // SetPriorityClass falla, el sondeo sigue con la prioridad de siempre.
    unsafe {
        use windows::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
        };
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}

/// Si [`run_if_requested`] lo activó y no se pidió [`ENV_IN_PROCESS`].
fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire) && !std::env::var(ENV_IN_PROCESS).is_ok_and(|v| v == "1")
}

/// Si cada apertura de Vulkan Video va en un hijo: en la aplicación y en el hijo del sondeo
/// entero, no en el hijo de una sola combinación.
fn isolation_enabled() -> bool {
    enabled() && (std::env::var(ENV_CHILD).is_err() || in_probe_all())
}

/// Si este proceso es el hijo del sondeo entero.
fn in_probe_all() -> bool {
    std::env::var(ENV_CHILD).is_ok_and(|order| order == PROBE_ALL)
}

/// El sondeo entero de [`Capabilities::probe`] en un proceso hijo con prioridad baja (ver el
/// módulo). `None` si el sondeo aislado no está activo o no se pudo lanzar el hijo: entonces se
/// sondea en este proceso. `Some(Err)` con el motivo si el hijo murió o no terminó.
pub(crate) fn isolated_probe() -> Option<Result<Capabilities, String>> {
    if !enabled() || std::env::var(ENV_CHILD).is_ok() {
        return None;
    }
    let (mut child, lines) = spawn(PROBE_ALL).ok()?;
    // Cada combinación ya tiene su tiempo límite: el hijo termina, como mucho, en la suma.
    let limit = super::probe::worst_case() + CHILD_TIMEOUT;
    let deadline = Instant::now() + limit;
    let result = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left) {
            Ok(Some(line)) => {
                if let Some(json) = line.strip_prefix(CAPS_LINE) {
                    break crate::caps::from_wire(json).ok_or_else(|| {
                        "el sondeo de códecs en un proceso aparte dio un resultado ilegible"
                            .to_owned()
                    });
                }
            }
            Ok(None) => {
                let status = child.wait().map(|status| describe_status(&status));
                break Err(format!(
                    "el sondeo de códecs en un proceso aparte terminó sin resultado ({})",
                    status.unwrap_or_else(|err| err.to_string())
                ));
            }
            Err(_) => {
                break Err(format!(
                    "el sondeo de códecs en un proceso aparte no terminó en {} s",
                    limit.as_secs()
                ));
            }
        }
    };
    // Con el resultado leído, el hijo ya no hace falta (aunque se trabe al salir).
    let _ = child.kill();
    let _ = child.wait();
    Some(result)
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
    let (mut child, lines) = spawn(order)?;
    let deadline = Instant::now() + CHILD_TIMEOUT;
    let mut verdict = String::new();
    loop {
        match lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Some(line)) => verdict = line,
            Ok(None) => break,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "el sondeo en un proceso aparte no respondió en {} s",
                    CHILD_TIMEOUT.as_secs()
                ));
            }
        }
    }
    let status = child
        .wait()
        .map_err(|err| format!("el proceso de sondeo no terminó bien: {err}"))?;
    let verdict = verdict.trim();
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

/// Lanza este ejecutable con la orden `order` y devuelve el hijo y sus líneas de salida, que
/// llegan a medida que las escribe (`None` al cerrarse la salida).
fn spawn(order: &str) -> Result<(Child, Receiver<Option<String>>), String> {
    let exe = std::env::current_exe()
        .map_err(|err| format!("no se sabe qué ejecutable relanzar para el sondeo: {err}"))?;
    let mut command = Command::new(exe);
    command
        .env(ENV_CHILD, order)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Sin ventana de consola propia para el hijo en Windows.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("no se pudo lanzar el proceso de sondeo: {err}"))?;
    let stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Some(stdout) = stdout {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(Some(line)).is_err() {
                    return;
                }
            }
        }
        let _ = tx.send(None);
    });
    Ok((child, rx))
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
