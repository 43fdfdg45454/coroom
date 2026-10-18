//! Registro: en Android, a logcat con la etiqueta `coroom`; en el equipo de desarrollo (pruebas),
//! a la salida de error. El nivel, con `RUST_LOG` (por defecto `info`).

use tracing_subscriber::EnvFilter;

/// Instala el registro del proceso (solo la primera vez).
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .without_time();
    #[cfg(target_os = "android")]
    let builder = builder.with_writer(logcat::Logcat);
    let _ = builder.try_init();
}

#[cfg(target_os = "android")]
mod logcat {
    //! Escritor de `tracing` hacia logcat: cada evento es una línea con su prioridad.

    #![allow(unsafe_code)]

    use std::ffi::CString;
    use std::io;

    use tracing_subscriber::fmt::MakeWriter;

    /// Etiqueta de logcat.
    const TAG: &std::ffi::CStr = c"coroom";

    /// Fábrica de escritores hacia logcat.
    pub struct Logcat;

    /// Un evento por escribir con su prioridad.
    pub struct Line {
        priority: ndk_sys::android_LogPriority,
        buffer: Vec<u8>,
    }

    impl<'a> MakeWriter<'a> for Logcat {
        type Writer = Line;

        fn make_writer(&'a self) -> Line {
            Line {
                priority: ndk_sys::android_LogPriority::ANDROID_LOG_INFO,
                buffer: Vec::new(),
            }
        }

        fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Line {
            use ndk_sys::android_LogPriority as P;
            let priority = match *meta.level() {
                tracing::Level::ERROR => P::ANDROID_LOG_ERROR,
                tracing::Level::WARN => P::ANDROID_LOG_WARN,
                tracing::Level::INFO => P::ANDROID_LOG_INFO,
                tracing::Level::DEBUG => P::ANDROID_LOG_DEBUG,
                tracing::Level::TRACE => P::ANDROID_LOG_VERBOSE,
            };
            Line {
                priority,
                buffer: Vec::new(),
            }
        }
    }

    impl io::Write for Line {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buffer.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Line {
        fn drop(&mut self) {
            let text = String::from_utf8_lossy(&self.buffer);
            let text = text.trim_end().replace('\0', " ");
            let Ok(text) = CString::new(text) else {
                return;
            };
            // SAFETY: `TAG` y `text` son cadenas C terminadas en NUL que viven durante la llamada;
            // `__android_log_write` no guarda los punteros.
            unsafe {
                ndk_sys::__android_log_write(self.priority.0 as i32, TAG.as_ptr(), text.as_ptr());
            }
        }
    }
}
