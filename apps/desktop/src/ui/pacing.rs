//! El ritmo de los cuadros de animación y lo que tarda en dibujarse la interfaz.
//!
//! Cada animación es una función del tiempo ([`super::app::App::now`]), así que se ve igual a 60,
//! 144 o 240 Hz y un cuadro perdido no la atrasa. Mientras algo se mueve
//! ([`super::app::App::animating`]) la ventana dibuja un cuadro por refresco del monitor en el que
//! está, y ninguno en reposo (salvo a pantalla completa en una sala solo de voz, que sigue el
//! refresco para que la sincronización adaptativa no baje el monitor al mínimo:
//! [`super::app::App::keeps_refresh`]). El ritmo lo marca el sistema, nunca un número supuesto
//! ([`FrameClock`]):
//!
//! - **Wayland**: la señal de cuadro del compositor. Justo antes de entregar la interfaz la ventana
//!   avisa (`pre_present_notify`) y `winit` retiene el siguiente `RedrawRequested` hasta que el
//!   compositor la manda: sigue la frecuencia real (también con VRR) y con la ventana oculta no
//!   llega.
//! - **Windows**: el reloj del compositor de escritorio, el próximo vblank que informa
//!   `DwmGetCompositionTimingInfo`.
//! - **X11 y los demás**: el período que informa el monitor de la ventana, releído al moverla. Si
//!   no lo informa, no se anima: el próximo dibujo enseña el estado final.
//!
//! Con el vídeo delante y llegando, la interfaz no agrega cuadros ([`NextFrame::WithVideo`]): el
//! próximo fotograma de vídeo la entrega y pide el cuadro siguiente; si el vídeo se detiene (nada
//! en el doble de su propio intervalo), se dibuja sola.
//!
//! [`DrawTimes`] mide cada dibujo de la interfaz (reconstruir, dibujar y entregar) para la etapa
//! «dibujado de la interfaz» del diagnóstico.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Quién marca el ritmo de los cuadros de animación (ver el módulo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClock {
    /// Wayland: la señal de cuadro del compositor.
    Compositor,
    /// Windows: el vblank del compositor de escritorio.
    Desktop,
    /// X11 y los demás: el período que informa el monitor (`None` si no lo informa).
    Monitor(Option<Duration>),
}

impl FrameClock {
    /// Período de un monitor que informa `millihertz` (`None` si no informa nada útil).
    pub fn period(millihertz: Option<u32>) -> Option<Duration> {
        millihertz
            .filter(|&mhz| mhz > 0)
            .map(|mhz| Duration::from_secs_f64(1000.0 / f64::from(mhz)))
    }
}

/// Cuándo dibujar el siguiente cuadro de animación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextFrame {
    /// Pedirlo ya: el compositor lo retiene hasta su señal de cuadro.
    Request,
    /// Despertar en ese instante y dibujarlo.
    At(Instant),
    /// Con el próximo fotograma de vídeo; si no llega antes de `fallback`, solo.
    WithVideo {
        /// Cuándo dibujar solo si el vídeo no llegó.
        fallback: Instant,
    },
    /// Nada marca el ritmo: no se anima.
    Never,
}

/// El siguiente cuadro mientras algo se mueve. `last` es cuándo empezó el último; `video`, el
/// intervalo del vídeo que se ve delante si sigue llegando; `vblank`, el próximo vblank del
/// compositor de escritorio ([`desktop_vblank`], solo con [`FrameClock::Desktop`]).
pub fn next_frame(
    clock: FrameClock,
    now: Instant,
    last: Instant,
    video: Option<Duration>,
    vblank: impl FnOnce(Instant) -> Option<Instant>,
) -> NextFrame {
    if let Some(interval) = video {
        return NextFrame::WithVideo {
            fallback: now + interval * 2,
        };
    }
    match clock {
        FrameClock::Compositor => NextFrame::Request,
        FrameClock::Desktop => vblank(now).map_or(NextFrame::Never, NextFrame::At),
        FrameClock::Monitor(Some(period)) => NextFrame::At((last + period).max(now)),
        FrameClock::Monitor(None) => NextFrame::Never,
    }
}

/// El próximo vblank del compositor de escritorio después de `now`.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn desktop_vblank(now: Instant) -> Option<Instant> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

    let mut info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    let (mut counter, mut frequency) = (0_i64, 0_i64);
    // SAFETY: las tres escriben solo en las variables locales que reciben; la ventana nula es la
    // que pide la API desde Windows 8.1 (el reloj del compositor, no el de una ventana).
    unsafe {
        DwmGetCompositionTimingInfo(HWND::default(), &mut info).ok()?;
        QueryPerformanceCounter(&mut counter).ok()?;
        QueryPerformanceFrequency(&mut frequency).ok()?;
    }
    let ahead = vblank_ahead(
        u64::try_from(counter).ok()?,
        info.qpcVBlank,
        info.qpcRefreshPeriod,
    )?;
    let frequency = u64::try_from(frequency).ok().filter(|&f| f > 0)?;
    Some(now + Duration::from_secs_f64(ahead as f64 / frequency as f64))
}

/// Sin compositor de escritorio que informe su vblank.
#[cfg(not(target_os = "windows"))]
pub fn desktop_vblank(_now: Instant) -> Option<Instant> {
    None
}

/// Cuentas del contador de rendimiento que faltan, desde `counter`, para el primer vblank
/// posterior (los vblanks caen en `vblank + k * period`).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn vblank_ahead(counter: u64, vblank: u64, period: u64) -> Option<u64> {
    if period == 0 {
        return None;
    }
    if vblank > counter {
        return Some(vblank - counter);
    }
    let elapsed = (counter - vblank) % period;
    Some(period - elapsed)
}

/// Lo que tardaron los últimos dibujos de la interfaz.
#[derive(Debug, Clone, Default)]
pub struct DrawTimes {
    samples: VecDeque<u64>,
}

impl DrawTimes {
    /// Dibujos que se recuerdan (unos segundos de animación a cualquier frecuencia).
    pub const WINDOW: usize = 600;

    /// Anota un dibujo.
    pub fn record(&mut self, took: Duration) {
        if self.samples.len() == Self::WINDOW {
            self.samples.pop_front();
        }
        self.samples
            .push_back(u64::try_from(took.as_micros()).unwrap_or(u64::MAX));
    }

    /// Dibujos anotados.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Si no hay ninguno.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Percentiles 50 y 95 en µs.
    pub fn p50_p95(&self) -> Option<(u64, u64)> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        Some((
            coroom_session::latency::nearest_rank(&sorted, 50.0),
            coroom_session::latency::nearest_rank(&sorted, 95.0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: Instant) -> Option<Instant> {
        None
    }

    #[test]
    fn the_monitor_sets_the_pace() {
        let now = Instant::now();
        let period = FrameClock::period(Some(144_000)).expect("período");
        assert!((period.as_secs_f64() - 1.0 / 144.0).abs() < 1e-9);
        assert_eq!(FrameClock::period(Some(0)), None);
        assert_eq!(FrameClock::period(None), None);
        let clock = FrameClock::Monitor(Some(period));
        assert_eq!(
            next_frame(clock, now, now, None, none),
            NextFrame::At(now + period)
        );
        // Un dibujo que tardó más que el período no acumula atraso: el siguiente, ya.
        let late = now + period * 3;
        assert_eq!(
            next_frame(clock, late, now, None, none),
            NextFrame::At(late)
        );
    }

    #[test]
    fn nothing_is_assumed_without_a_clock() {
        let now = Instant::now();
        assert_eq!(
            next_frame(FrameClock::Monitor(None), now, now, None, none),
            NextFrame::Never
        );
        assert_eq!(
            next_frame(FrameClock::Desktop, now, now, None, none),
            NextFrame::Never
        );
        let vblank = now + Duration::from_millis(3);
        assert_eq!(
            next_frame(FrameClock::Desktop, now, now, None, |_| Some(vblank)),
            NextFrame::At(vblank)
        );
        assert_eq!(
            next_frame(FrameClock::Compositor, now, now, None, none),
            NextFrame::Request
        );
    }

    #[test]
    fn with_video_the_ui_rides_its_frames() {
        let now = Instant::now();
        let interval = Duration::from_micros(16_667);
        for clock in [
            FrameClock::Compositor,
            FrameClock::Desktop,
            FrameClock::Monitor(None),
        ] {
            assert_eq!(
                next_frame(clock, now, now, Some(interval), none),
                NextFrame::WithVideo {
                    fallback: now + interval * 2
                }
            );
        }
    }

    #[test]
    fn the_next_vblank_follows_the_desktop_clock() {
        assert_eq!(vblank_ahead(1_000, 1_250, 100), Some(250));
        assert_eq!(vblank_ahead(1_000, 900, 100), Some(100), "justo en uno");
        assert_eq!(vblank_ahead(1_030, 900, 100), Some(70));
        assert_eq!(vblank_ahead(1_000, 900, 0), None);
    }

    #[test]
    fn draw_times_keep_a_window() {
        let mut times = DrawTimes::default();
        assert_eq!(times.p50_p95(), None);
        for us in 1..=100 {
            times.record(Duration::from_micros(us));
        }
        assert_eq!(times.p50_p95(), Some((50, 95)));
        for _ in 0..DrawTimes::WINDOW {
            times.record(Duration::from_micros(7));
        }
        assert_eq!(times.len(), DrawTimes::WINDOW);
        assert_eq!(times.p50_p95(), Some((7, 7)));
    }
}
