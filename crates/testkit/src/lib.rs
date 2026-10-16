//! Ayudas para las pruebas de coroom (solo `dev-dependencies`).
//!
//! Las pruebas se dividen en dos clases:
//!
//! - **Funcionales**: si fallan, la aplicación no funciona. Son `assert!` normales y hacen fallar
//!   la CI.
//! - **De tiempo**: latencias, ritmos, bitrates, huecos o cuotas que dependen de cuánto tarda la
//!   máquina (en un runner cargado, el sistema despierta un hilo varios ms tarde sin que el código
//!   tenga la culpa). Van con [`timing_assert!`]: si se salen del umbral, avisan y la prueba sigue.
//!
//! Cada aviso se escribe en la salida de error y, si `COROOM_TIMING_REPORT` nombra un fichero, se
//! añade allí como una línea `fichero:línea<TAB>prueba<TAB>mensaje`; la CI lo convierte en
//! anotaciones y en un resumen. Con `COROOM_TIMING_STRICT=1` un aviso hace fallar la prueba (para
//! medir en una máquina tranquila).

use std::io::Write;

/// Comprueba un umbral de tiempo: si `cond` es falsa, avisa (ver [`timing_failed`]) en lugar de
/// hacer fallar la prueba. Devuelve si se cumplió.
///
/// ```
/// let p95_ms = 12.0;
/// coroom_testkit::timing_assert!(p95_ms < 20.0, "p95 de {p95_ms} ms");
/// ```
#[macro_export]
macro_rules! timing_assert {
    ($cond:expr $(,)?) => {
        $crate::timing_assert!($cond, "{}", stringify!($cond))
    };
    ($cond:expr, $($arg:tt)+) => {{
        let ok: bool = $cond;
        if !ok {
            $crate::timing_failed(file!(), line!(), &format!($($arg)+));
        }
        ok
    }};
}

/// Registra que una comprobación de tiempo se salió de su umbral.
pub fn timing_failed(file: &str, line: u32, message: &str) {
    let report = std::env::var_os("COROOM_TIMING_REPORT").map(std::path::PathBuf::from);
    let strict = std::env::var_os("COROOM_TIMING_STRICT").is_some_and(|value| value != "0");
    record(report.as_deref(), strict, file, line, message);
}

fn record(report: Option<&std::path::Path>, strict: bool, file: &str, line: u32, message: &str) {
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("?");
    // Una línea por aviso: los saltos del mensaje no la parten.
    let message = message.replace(['\n', '\r', '\t'], " ");
    eprintln!("aviso de tiempo en {file}:{line} ({test}): {message}");
    if let Some(path) = report {
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            // Una sola escritura por línea: varias pruebas en paralelo añaden al mismo informe.
            .and_then(|mut report| {
                report.write_all(format!("{file}:{line}\t{test}\t{message}\n").as_bytes())
            });
        if let Err(error) = written {
            eprintln!(
                "no se pudo escribir el aviso en {}: {error}",
                path.display()
            );
        }
    }
    if strict {
        panic!("umbral de tiempo superado en {file}:{line}: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::record;

    #[test]
    fn a_missed_threshold_is_one_report_line() {
        let report = std::env::temp_dir().join(format!("coroom-timing-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&report);
        record(Some(&report), false, "a.rs", 7, "p95 de 3 ms\ncon salto");
        record(Some(&report), false, "b.rs", 9, "otro");
        let written = std::fs::read_to_string(&report).expect("informe");
        let _ = std::fs::remove_file(&report);
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2, "{written}");
        let fields: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(fields[0], "a.rs:7");
        assert!(
            fields[1].contains("a_missed_threshold_is_one_report_line"),
            "{written}"
        );
        assert_eq!(fields[2], "p95 de 3 ms con salto");
    }

    #[test]
    fn a_met_threshold_returns_true_and_reports_nothing() {
        // El caso que no se cumple escribiría en el informe de la CI: lo cubren las pruebas de
        // `record`.
        assert!(timing_assert!(1 < 2, "nunca"));
        assert!(timing_assert!(2 > 1));
    }

    #[test]
    #[should_panic(expected = "umbral de tiempo superado en c.rs:1")]
    fn strict_mode_fails_the_test() {
        record(None, true, "c.rs", 1, "lento");
    }
}
