//! Prioridad de tiempo real para los hilos de audio de coroom.
//!
//! Los hilos de los dispositivos ya la tienen: el de datos de PipeWire, por su `module-rt`, y ahí
//! corre el callback de cada flujo de coroom (se conectan con `PW_STREAM_FLAG_RT_PROCESS`, ver
//! el módulo `pipewire`), y el de cada flujo WASAPI, que pide MMCSS con esta función. El resto son
//! los nuestros: recepción y descifrado del audio, captura y codificación, y la reproducción con
//! cola. Con la CPU cargada por un juego, un hilo normal puede esperar su turno más de una trama
//! de audio (10 ms): eso es jitter que el buffer tiene que absorber con más cola, o un hueco que
//! se oye.
//!
//! - **Linux**: `SCHED_FIFO` directo si el límite `RLIMIT_RTPRIO` lo permite; si no, a través del
//!   portal `org.freedesktop.portal.Realtime` (dentro de Flatpak) o de RealtimeKit
//!   (`org.freedesktop.RealtimeKit1`, en el bus del sistema), como hace PipeWire. Antes se fija
//!   `RLIMIT_RTTIME` al máximo que admiten: un hilo que acapara la CPU pierde la prioridad en vez
//!   de colgar el equipo. Siempre con `SCHED_RESET_ON_FORK`.
//! - **Windows**: MMCSS, tarea «Pro Audio» (`AvSetMmThreadCharacteristicsW`) con prioridad alta.
//! - **Android** (feature `aaudio`): el hilo del dispositivo es el del callback de AAudio, que el
//!   sistema ya pone en `SCHED_FIFO`; los nuestros pasan al *nice* de los hilos de audio de una
//!   aplicación, `THREAD_PRIORITY_AUDIO` (−16, lo mismo que `Process.setThreadPriority`), que
//!   Android permite sin privilegios. Ni RealtimeKit ni el portal: no existen allí.
//!
//! Si no se concede, el hilo sigue con prioridad normal: el audio funciona igual, con más jitter
//! cuando la máquina va cargada.

/// Prioridad pedida (`SCHED_FIFO`, 1-99): por debajo del hilo de datos de PipeWire, que es el que
/// marca el ritmo del dispositivo. RealtimeKit la recorta a su máximo (20 por defecto).
pub const PRIORITY: u32 = 10;

/// Sube el hilo que llama a tiempo real. Devuelve por qué vía se concedió o el motivo de no.
pub fn promote_current_thread() -> Result<&'static str, String> {
    imp::promote(PRIORITY)
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)] // libc sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
mod imp {
    use zbus::blocking::{Connection, Proxy};

    pub(super) fn promote(priority: u32) -> Result<&'static str, String> {
        if set_fifo(priority).is_ok() {
            return Ok("SCHED_FIFO");
        }
        // SAFETY: `gettid` no tiene precondiciones y no puede fallar.
        let tid = u64::try_from(unsafe { libc::gettid() }).map_err(|err| err.to_string())?;
        if std::path::Path::new("/.flatpak-info").exists() {
            portal(tid, priority).map(|()| "portal Realtime")
        } else {
            rtkit(tid, priority).map(|()| "RealtimeKit")
        }
    }

    fn set_fifo(priority: u32) -> Result<(), std::io::Error> {
        let param = libc::sched_param {
            sched_priority: priority as libc::c_int,
        };
        // SAFETY: `param` vive durante la llamada; el pid 0 es el hilo que llama (en Linux la
        // planificación es por hilo).
        let rc = unsafe {
            libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param)
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Deja `RLIMIT_RTTIME` (µs de CPU seguidos en tiempo real) en `max_us` si estaba por encima
    /// o sin límite: RealtimeKit y el portal no conceden nada sin él.
    fn limit_rttime(max_us: u64) -> Result<(), String> {
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `current` vive durante la llamada y es del tipo que espera.
        if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut current) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let max = max_us as libc::rlim_t;
        if current.rlim_max != libc::RLIM_INFINITY && current.rlim_max <= max {
            return Ok(());
        }
        let limit = libc::rlimit {
            rlim_cur: current.rlim_cur.min(max),
            rlim_max: max,
        };
        // SAFETY: `limit` vive durante la llamada; solo baja el límite (no hace falta privilegio).
        if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &limit) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }

    /// Lo que ofrecen RealtimeKit y el portal: prioridad máxima y `RTTimeUSecMax`.
    fn limits(proxy: &Proxy<'_>) -> Result<(u32, u64), String> {
        let max: i32 = proxy
            .get_property("MaxRealtimePriority")
            .map_err(|err| err.to_string())?;
        let rttime: i64 = proxy
            .get_property("RTTimeUSecMax")
            .map_err(|err| err.to_string())?;
        Ok((max.max(1) as u32, rttime.max(1) as u64))
    }

    fn rtkit(tid: u64, priority: u32) -> Result<(), String> {
        let bus = Connection::system().map_err(|err| format!("bus del sistema: {err}"))?;
        let proxy = Proxy::new(
            &bus,
            "org.freedesktop.RealtimeKit1",
            "/org/freedesktop/RealtimeKit1",
            "org.freedesktop.RealtimeKit1",
        )
        .map_err(|err| err.to_string())?;
        let (max, rttime) = limits(&proxy).map_err(|err| format!("RealtimeKit: {err}"))?;
        limit_rttime(rttime)?;
        proxy
            .call::<_, _, ()>("MakeThreadRealtime", &(tid, priority.min(max)))
            .map_err(|err| format!("RealtimeKit: {err}"))
    }

    fn portal(tid: u64, priority: u32) -> Result<(), String> {
        let bus = Connection::session().map_err(|err| format!("bus de sesión: {err}"))?;
        let proxy = Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Realtime",
        )
        .map_err(|err| err.to_string())?;
        let (max, rttime) = limits(&proxy).map_err(|err| format!("portal Realtime: {err}"))?;
        limit_rttime(rttime)?;
        let pid = u64::from(std::process::id());
        proxy
            .call::<_, _, ()>("MakeThreadRealtimeWithPID", &(pid, tid, priority.min(max)))
            .map_err(|err| format!("portal Realtime: {err}"))
    }
}

#[cfg(all(target_os = "windows", feature = "wasapi"))]
#[allow(unsafe_code)] // MMCSS (avrt.dll) sin envoltorio seguro; cada bloque lleva su `SAFETY:`.
mod imp {
    use windows::Win32::System::Threading::{
        AVRT_PRIORITY_HIGH, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
    };
    use windows::core::w;

    pub(super) fn promote(_priority: u32) -> Result<&'static str, String> {
        let mut index = 0u32;
        // SAFETY: el nombre es una cadena constante terminada en cero e `index` vive durante la
        // llamada. El handle queda asociado al hilo: Windows lo suelta cuando el hilo termina.
        let task = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) }
            .map_err(|err| format!("MMCSS: {err}"))?;
        // SAFETY: `task` es el handle que acaba de dar `AvSetMmThreadCharacteristicsW`.
        unsafe { AvSetMmThreadPriority(task, AVRT_PRIORITY_HIGH) }
            .map_err(|err| format!("MMCSS: {err}"))?;
        Ok("MMCSS «Pro Audio»")
    }
}

#[cfg(all(target_os = "android", feature = "aaudio"))]
mod imp {
    /// `android.os.Process.THREAD_PRIORITY_AUDIO`: el *nice* de los hilos de audio.
    const THREAD_PRIORITY_AUDIO: i32 = -16;

    pub(super) fn promote(_priority: u32) -> Result<&'static str, String> {
        let tid = rustix::thread::gettid();
        rustix::process::setpriority_process(Some(tid), THREAD_PRIORITY_AUDIO)
            .map(|()| "nice −16 (THREAD_PRIORITY_AUDIO)")
            .map_err(|err| format!("setpriority: {err}"))
    }
}

#[cfg(not(any(
    target_os = "linux",
    all(target_os = "windows", feature = "wasapi"),
    all(target_os = "android", feature = "aaudio")
)))]
mod imp {
    pub(super) fn promote(_priority: u32) -> Result<&'static str, String> {
        Err("sin prioridad de tiempo real en esta plataforma".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promotion_never_panics_and_explains_a_refusal() {
        // En la CI y en los contenedores no suele haber RealtimeKit ni límite de RTPRIO: lo que
        // importa es que no falle de otra forma y que diga por qué.
        let handle = std::thread::spawn(promote_current_thread);
        match handle.join().expect("el hilo no entra en pánico") {
            Ok(via) => assert!(!via.is_empty()),
            Err(reason) => assert!(!reason.is_empty()),
        }
    }
}
