//! Cuentas de los backends por ráfagas (AAudio, ver el módulo `aaudio`) que no dependen de la
//! plataforma, para probarlas sin el dispositivo:
//!
//! - [`BufferTuner`]: el tamaño del búfer de reproducción. Empieza en **una ráfaga** (las tramas
//!   que el dispositivo mueve de una vez, `AAudioStream_getFramesPerBurst`) y crece una ráfaga
//!   cada vez que el contador de faltas del propio flujo (`AAudioStream_getXRunCount`) sube, hasta
//!   la capacidad que dice el flujo. Es el ajuste que recomienda la guía de AAudio para baja
//!   latencia: ningún tamaño supuesto, solo lo que se mide.
//! - [`time_until_presented`] y [`time_since_captured`]: cuándo suena o se capturó una trama con la
//!   marca del dispositivo (`AAudioStream_getTimestamp(CLOCK_MONOTONIC)`: la trama `frame_position`
//!   sonó o se capturó en `time_nanoseconds`), contando las tramas de diferencia a la frecuencia del
//!   flujo.
//! - [`capture_backlog`]: cuánto atraso de captura descartar para que la latencia no crezca si el
//!   hilo que lee se retrasa (el mismo límite que el anillo de los otros backends).
//! - [`frames_or_error`]: el resultado de las llamadas de AAudio que devuelven tramas, y
//!   [`parse_device_id`], el id de un dispositivo de Android en texto.

use std::time::Duration;

/// Nanosegundos por segundo.
const NANOS_PER_SEC: i128 = 1_000_000_000;

/// Tamaño del búfer de un flujo de reproducción por ráfagas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BufferTuner {
    /// Lo que crece en cada ajuste: una ráfaga.
    step: usize,
    /// Lo más que admite el flujo.
    capacity: usize,
    /// Tamaño actual (el que concedió el flujo la última vez).
    size: usize,
    /// Faltas del flujo ya vistas.
    xruns: u64,
}

impl BufferTuner {
    /// Para un flujo con ráfagas de `burst` tramas, una capacidad de `capacity` y un búfer de
    /// `current` al abrirlo (lo que se usa de paso si el flujo no dice su ráfaga). Una capacidad
    /// de 0 (que el flujo no la diga) no limita.
    pub(crate) fn new(burst: usize, capacity: usize, current: usize) -> Self {
        let step = if burst > 0 { burst } else { current.max(1) };
        let capacity = if capacity > 0 { capacity } else { usize::MAX };
        Self {
            step,
            capacity,
            size: step.min(capacity),
            xruns: 0,
        }
    }

    /// Tamaño que se pide al flujo ahora (al abrir: una ráfaga).
    pub(crate) fn size(&self) -> usize {
        self.size
    }

    /// Lo que concedió el flujo al pedirle [`BufferTuner::size`] (puede redondear).
    pub(crate) fn granted(&mut self, frames: usize) {
        if frames > 0 {
            self.size = frames.min(self.capacity);
        }
    }

    /// El flujo lleva `xruns` faltas desde que se abrió: si subieron, una ráfaga más (hasta la
    /// capacidad). Devuelve el tamaño nuevo que hay que pedir, o `None` si no cambia.
    pub(crate) fn observe(&mut self, xruns: u64) -> Option<usize> {
        if xruns <= self.xruns {
            return None;
        }
        self.xruns = xruns;
        if self.size >= self.capacity {
            return None;
        }
        self.size = self.size.saturating_add(self.step).min(self.capacity);
        Some(self.size)
    }
}

/// Instante (ns del reloj monótono) de la trama `frame`, sabiendo que la trama `stamp_frame` es
/// la del instante `stamp_ns`. `None` con una frecuencia de 0.
fn frame_time_ns(stamp_frame: i64, stamp_ns: i64, frame: i64, rate: u32) -> Option<i128> {
    if rate == 0 {
        return None;
    }
    let frames = i128::from(frame) - i128::from(stamp_frame);
    Some(i128::from(stamp_ns) + frames * NANOS_PER_SEC / i128::from(rate))
}

fn nanos(value: i128) -> Duration {
    Duration::from_nanos(u64::try_from(value.max(0)).unwrap_or(u64::MAX))
}

/// Salida: cuánto falta (desde `now_ns`) para que suene la trama `frame`, con la marca del
/// dispositivo (`stamp_frame` sonó en `stamp_ns`, los dos instantes del reloj monótono). Lo que ya
/// debería haber sonado cuenta como cero.
pub(crate) fn time_until_presented(
    stamp_frame: i64,
    stamp_ns: i64,
    frame: i64,
    now_ns: i64,
    rate: u32,
) -> Option<Duration> {
    let at = frame_time_ns(stamp_frame, stamp_ns, frame, rate)?;
    Some(nanos(at - i128::from(now_ns)))
}

/// Entrada: hace cuánto (hasta `now_ns`) se capturó la trama `frame`, con la marca del
/// dispositivo (`stamp_frame` se capturó en `stamp_ns`). Una trama «del futuro» (una marca
/// adelantada) cuenta como recién capturada.
pub(crate) fn time_since_captured(
    stamp_frame: i64,
    stamp_ns: i64,
    frame: i64,
    now_ns: i64,
    rate: u32,
) -> Option<Duration> {
    let at = frame_time_ns(stamp_frame, stamp_ns, frame, rate)?;
    Some(nanos(i128::from(now_ns) - at))
}

/// Captura: tramas que descartar antes de leer la siguiente trama de `frame` tramas cuando el
/// dispositivo ya tiene `available` esperando. Con más de tres tramas, lo más antiguo hasta dejar
/// dos: la que se lee y una detrás, como el anillo de los otros backends (`ring.rs`). Así un hilo
/// que se retrasa pierde audio viejo en vez de acumular latencia.
pub(crate) fn capture_backlog(available: i64, frame: usize) -> usize {
    let available = usize::try_from(available).unwrap_or(0);
    if frame == 0 || available <= frame * 3 {
        0
    } else {
        available - frame * 2
    }
}

/// Lo que devuelven `AAudioStream_read`, `AAudioStream_write` y
/// `AAudioStream_setBufferSizeInFrames`: tramas si no es negativo; si lo es, el código de error
/// de AAudio. `ndk` 0.9 toma por error cualquier valor distinto de cero (`AudioError::from_result`),
/// así que el backend vuelve a mirar el código aquí.
pub(crate) fn frames_or_error(code: i32) -> Result<usize, i32> {
    usize::try_from(code).map_err(|_| code)
}

/// El id de un dispositivo de Android (`AudioDeviceInfo.getId()`, siempre positivo) escrito en
/// texto, como llega de `--speaker` o `--mic`; `None` si no lo es.
pub(crate) fn parse_device_id(name: &str) -> Option<i32> {
    name.trim().parse::<i32>().ok().filter(|id| *id > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_starts_at_one_burst_and_grows_only_on_new_xruns() {
        let mut tuner = BufferTuner::new(96, 960, 1920);
        assert_eq!(tuner.size(), 96);
        // Sin faltas, no cambia; la misma cuenta otra vez, tampoco.
        assert_eq!(tuner.observe(0), None);
        assert_eq!(tuner.observe(1), Some(192));
        assert_eq!(tuner.observe(1), None);
        // Varias faltas de golpe: una ráfaga por ajuste, no una por falta.
        assert_eq!(tuner.observe(5), Some(288));
        assert_eq!(tuner.size(), 288);
    }

    #[test]
    fn the_buffer_never_passes_the_capacity() {
        let mut tuner = BufferTuner::new(192, 400, 0);
        assert_eq!(tuner.observe(1), Some(384));
        assert_eq!(tuner.observe(2), Some(400));
        assert_eq!(tuner.observe(3), None);
        assert_eq!(tuner.size(), 400);
    }

    #[test]
    fn the_granted_size_is_what_counts() {
        // El flujo redondea lo pedido (a ráfagas enteras, a su mínimo): lo siguiente crece desde
        // lo concedido.
        let mut tuner = BufferTuner::new(96, 1000, 0);
        tuner.granted(192);
        assert_eq!(tuner.size(), 192);
        assert_eq!(tuner.observe(1), Some(288));
        tuner.granted(0);
        assert_eq!(tuner.size(), 288);
    }

    #[test]
    fn without_a_burst_the_current_buffer_is_the_step_and_no_capacity_is_unbounded() {
        let mut tuner = BufferTuner::new(0, 0, 240);
        assert_eq!(tuner.size(), 240);
        assert_eq!(tuner.observe(1), Some(480));
    }

    #[test]
    fn output_frames_are_presented_after_what_is_already_queued() {
        // La trama 1000 sonó en t = 5 s; la 1480 (480 más) suena 10 ms después.
        let stamp_ns = 5_000_000_000;
        let delay = time_until_presented(1_000, stamp_ns, 1_480, stamp_ns, 48_000);
        assert_eq!(delay, Some(Duration::from_millis(10)));
        // 4 ms después de la marca quedan 6.
        let delay = time_until_presented(1_000, stamp_ns, 1_480, stamp_ns + 4_000_000, 48_000);
        assert_eq!(delay, Some(Duration::from_millis(6)));
        // Lo que ya tendría que haber sonado: cero, no negativo.
        let late = time_until_presented(1_000, stamp_ns, 1_000, stamp_ns + 1_000_000, 48_000);
        assert_eq!(late, Some(Duration::ZERO));
        assert_eq!(time_until_presented(0, 0, 1, 0, 0), None);
    }

    #[test]
    fn input_frames_are_dated_from_the_device_stamp() {
        // La trama 9600 se capturó en t = 2 s; la 9120 (480 antes), 10 ms antes.
        let stamp_ns = 2_000_000_000;
        let age = time_since_captured(9_600, stamp_ns, 9_120, stamp_ns, 48_000);
        assert_eq!(age, Some(Duration::from_millis(10)));
        let age = time_since_captured(9_600, stamp_ns, 9_600, stamp_ns + 3_000_000, 48_000);
        assert_eq!(age, Some(Duration::from_millis(3)));
        // Una marca por delante del reloj no da edades negativas.
        let ahead = time_since_captured(9_600, stamp_ns, 10_080, stamp_ns, 48_000);
        assert_eq!(ahead, Some(Duration::ZERO));
    }

    #[test]
    fn a_capture_backlog_is_trimmed_to_two_frames() {
        assert_eq!(capture_backlog(0, 480), 0);
        assert_eq!(capture_backlog(-5, 480), 0);
        assert_eq!(capture_backlog(1_440, 480), 0);
        assert_eq!(capture_backlog(1_441, 480), 481);
        assert_eq!(capture_backlog(4_800, 480), 3_840);
        assert_eq!(capture_backlog(100, 0), 0);
    }

    #[test]
    fn aaudio_counts_are_frames_and_negatives_are_errors() {
        assert_eq!(frames_or_error(0), Ok(0));
        assert_eq!(frames_or_error(480), Ok(480));
        // AAUDIO_ERROR_DISCONNECTED.
        assert_eq!(frames_or_error(-899), Err(-899));
    }

    #[test]
    fn android_device_ids_are_positive_numbers() {
        assert_eq!(parse_device_id("12"), Some(12));
        assert_eq!(parse_device_id(" 7 "), Some(7));
        assert_eq!(parse_device_id("0"), None);
        assert_eq!(parse_device_id("-3"), None);
        assert_eq!(parse_device_id("auriculares"), None);
    }
}
