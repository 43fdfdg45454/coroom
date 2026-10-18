//! La `Surface` de la vista del vídeo y los fotogramas que MediaCodec deja en ella.
//!
//! - [`OutputSurface`]: la `Surface` de la `SurfaceView` (en Android, su `ANativeWindow`).
//! - [`SurfaceSlot`]: el sitio compartido donde la aplicación pone la superficie y la quita. Los
//!   decodificadores abiertos se enteran en el acto (`AMediaCodec_setOutputSurface`, sin volver a
//!   crear el códec) y, sin superficie, pasan a una de reserva fuera de la pantalla y sueltan los
//!   fotogramas sin dibujarlos.
//! - [`SurfaceFrame`]: un búfer de salida decodificado que la presentación suelta a la pantalla
//!   ([`SurfaceFrame::present`]) o descarta; si nadie lo hace, al soltar el último `Arc` vuelve al
//!   decodificador sin dibujarse: un fotograma descartado nunca deja al códec sin búferes.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Instant;

/// La `Surface` de la vista del vídeo.
#[derive(Clone, PartialEq, Eq)]
pub struct OutputSurface {
    #[cfg(target_os = "android")]
    window: ndk::native_window::NativeWindow,
    /// Fuera de Android solo existe en las pruebas: una identidad sin ventana.
    #[cfg(not(target_os = "android"))]
    id: u64,
}

impl OutputSurface {
    /// La superficie de la `ANativeWindow` de una `Surface` de Java (la aplicación la obtiene con
    /// `ANativeWindow_fromSurface`, `NativeWindow::from_surface` en la crate `ndk`).
    #[cfg(target_os = "android")]
    pub fn new(window: ndk::native_window::NativeWindow) -> Self {
        Self { window }
    }

    /// La ventana nativa.
    #[cfg(target_os = "android")]
    pub fn window(&self) -> &ndk::native_window::NativeWindow {
        &self.window
    }

    /// Una superficie sin ventana para las pruebas del equipo de desarrollo.
    #[cfg(all(test, not(target_os = "android")))]
    pub(crate) fn fake(id: u64) -> Self {
        Self { id }
    }
}

impl fmt::Debug for OutputSurface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(target_os = "android")]
        {
            write!(f, "OutputSurface({:?})", self.window.ptr())
        }
        #[cfg(not(target_os = "android"))]
        {
            write!(f, "OutputSurface(#{})", self.id)
        }
    }
}

/// Quien usa la superficie del sitio (los decodificadores abiertos).
pub(crate) trait SurfaceUser: Send + Sync {
    /// Cambió la superficie (`None`: ya no hay; hay que dejar de usar la anterior **antes de
    /// volver**, la aplicación la destruye en cuanto vuelve `surfaceDestroyed`).
    fn surface_changed(&self, surface: Option<&OutputSurface>);
}

#[derive(Default)]
struct SlotState {
    surface: Option<OutputSurface>,
    users: Vec<Weak<dyn SurfaceUser>>,
}

#[derive(Default)]
struct SlotInner {
    state: Mutex<SlotState>,
    /// Si hay superficie en pantalla: la presentación lo mira sin bloquear.
    visible: AtomicBool,
}

/// Sitio compartido de la superficie de la vista del vídeo, que actualiza la aplicación.
///
/// Desde Kotlin (`SurfaceHolder.Callback`): [`SurfaceSlot::set`] en `surfaceCreated` (y en
/// `surfaceChanged` si la `Surface` es otra), [`SurfaceSlot::clear`] **dentro** de
/// `surfaceDestroyed` (vuelve cuando ningún decodificador la usa ya). Girar la pantalla con la
/// misma `Surface` no hace nada: el tamaño de la vista no le importa al decodificador.
///
/// Orden de cerrojos: el del sitio va siempre antes que el del decodificador.
#[derive(Clone, Default)]
pub struct SurfaceSlot {
    inner: Arc<SlotInner>,
}

impl SurfaceSlot {
    /// Un sitio vacío.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pone `surface`: los decodificadores abiertos pasan a dibujar en ella (sin recrearse).
    pub fn set(&self, surface: OutputSurface) {
        let mut state = self.lock();
        if state.surface.as_ref() != Some(&surface) {
            for user in live_users(&mut state.users) {
                user.surface_changed(Some(&surface));
            }
            state.surface = Some(surface);
        }
        self.inner.visible.store(true, Ordering::Release);
    }

    /// Quita la superficie: los fotogramas se sueltan sin dibujar desde ya y los decodificadores
    /// pasan a la de reserva antes de volver.
    pub fn clear(&self) {
        let mut state = self.lock();
        self.inner.visible.store(false, Ordering::Release);
        if state.surface.take().is_some() {
            for user in live_users(&mut state.users) {
                user.surface_changed(None);
            }
        }
    }

    /// La superficie actual.
    pub fn surface(&self) -> Option<OutputSurface> {
        self.lock().surface.clone()
    }

    /// Si hay superficie en pantalla.
    pub fn is_visible(&self) -> bool {
        self.inner.visible.load(Ordering::Acquire)
    }

    /// Si `self` y `other` son el mismo sitio.
    pub fn same(&self, other: &SurfaceSlot) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Ejecuta `f` con la superficie actual y el cerrojo del sitio tomado (nadie la cambia
    /// mientras tanto); si devuelve un usuario, queda apuntado para enterarse de los cambios.
    pub(crate) fn attach_with<T, E>(
        &self,
        f: impl FnOnce(Option<&OutputSurface>) -> Result<(T, Arc<dyn SurfaceUser>), E>,
    ) -> Result<T, E> {
        let mut state = self.lock();
        let (value, user) = f(state.surface.as_ref())?;
        state.users.push(Arc::downgrade(&user));
        Ok(value)
    }

    /// Ejecuta `f` con la superficie actual y el cerrojo del sitio tomado.
    pub(crate) fn with_surface<T>(&self, f: impl FnOnce(Option<&OutputSurface>) -> T) -> T {
        let state = self.lock();
        f(state.surface.as_ref())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Los usuarios que siguen vivos (y olvida los demás).
fn live_users(users: &mut Vec<Weak<dyn SurfaceUser>>) -> Vec<Arc<dyn SurfaceUser>> {
    users.retain(|user| user.strong_count() > 0);
    users.iter().filter_map(Weak::upgrade).collect()
}

impl fmt::Debug for SurfaceSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SurfaceSlot")
            .field("visible", &self.is_visible())
            .finish_non_exhaustive()
    }
}

/// Cómo se suelta un búfer de salida.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Release {
    /// Sin dibujar (`releaseOutputBuffer(render = false)`).
    Discard,
    /// A la pantalla en cuanto se pueda (`releaseOutputBuffer(render = true)`: el compositor lo
    /// toma en el siguiente vsync).
    Render,
    /// A la pantalla en el vsync de este instante o el siguiente (`releaseOutputBufferAtTime`,
    /// nanosegundos de `CLOCK_MONOTONIC`, los de `System.nanoTime()` y `Choreographer`).
    RenderAt(i64),
}

/// Qué pasó al soltar un fotograma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentOutcome {
    /// Va a la pantalla.
    Shown,
    /// No hay superficie en pantalla (aplicación en segundo plano): se soltó sin dibujar.
    Hidden,
    /// Se descartó a propósito ([`SurfaceFrame::discard`]).
    Discarded,
    /// El búfer ya no era suyo: el decodificador se vació o se cerró después de entregarlo, o el
    /// fotograma ya se había soltado.
    Stale,
    /// MediaCodec dio un error al soltarlo.
    Failed(String),
}

impl PresentOutcome {
    /// Si llegó a la pantalla.
    pub fn shown(&self) -> bool {
        matches!(self, PresentOutcome::Shown)
    }
}

/// Dueño de los búferes de salida (el decodificador).
pub(crate) trait OutputOwner: Send + Sync {
    /// Suelta el búfer `index` entregado en la generación `generation` del códec.
    fn release(&self, index: usize, generation: u64, how: Release) -> PresentOutcome;
}

/// Un búfer de salida de MediaCodec ya decodificado en la superficie, pendiente de soltar.
pub struct SurfaceFrame {
    owner: Arc<dyn OutputOwner>,
    index: usize,
    generation: u64,
    released: AtomicBool,
    width: u32,
    height: u32,
    pts_us: u64,
    decoded_at: Instant,
}

impl SurfaceFrame {
    pub(crate) fn new(
        owner: Arc<dyn OutputOwner>,
        index: usize,
        generation: u64,
        (width, height): (u32, u32),
        pts_us: u64,
        decoded_at: Instant,
    ) -> Self {
        Self {
            owner,
            index,
            generation,
            released: AtomicBool::new(false),
            width,
            height,
            pts_us,
            decoded_at,
        }
    }

    /// Lo suelta a la pantalla en cuanto se pueda (la ruta de menor latencia). Sin superficie en
    /// pantalla, lo suelta sin dibujar ([`PresentOutcome::Hidden`]).
    pub fn present(&self) -> PresentOutcome {
        self.release(Release::Render)
    }

    /// Lo suelta para el vsync de `monotonic_ns` (`CLOCK_MONOTONIC`, el reloj de
    /// `System.nanoTime()` y de `Choreographer.FrameCallback`) o el siguiente. Android exige que
    /// esté a menos de un segundo de ahora.
    pub fn present_at(&self, monotonic_ns: i64) -> PresentOutcome {
        self.release(Release::RenderAt(monotonic_ns))
    }

    /// Lo devuelve al decodificador sin dibujarlo.
    pub fn discard(&self) -> PresentOutcome {
        self.release(Release::Discard)
    }

    /// Tamaño visible.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Marca de tiempo con la que se encoló la entrada (µs).
    pub fn pts_us(&self) -> u64 {
        self.pts_us
    }

    /// Cuándo salió del decodificador.
    pub fn decoded_at(&self) -> Instant {
        self.decoded_at
    }

    /// Si ya se soltó (presentado o descartado).
    pub fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    fn release(&self, how: Release) -> PresentOutcome {
        if self.released.swap(true, Ordering::AcqRel) {
            return PresentOutcome::Stale;
        }
        self.owner.release(self.index, self.generation, how)
    }
}

impl Drop for SurfaceFrame {
    fn drop(&mut self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            let _ = self
                .owner
                .release(self.index, self.generation, Release::Discard);
        }
    }
}

impl fmt::Debug for SurfaceFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SurfaceFrame")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .field("size", &(self.width, self.height))
            .field("pts_us", &self.pts_us)
            .field("released", &self.is_released())
            .finish_non_exhaustive()
    }
}

// Con superficies y códecs falsos: solo en el equipo de desarrollo.
#[cfg(test)]
#[cfg(not(target_os = "android"))]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<Option<u64>>>,
        released: Mutex<Vec<(usize, u64, Release)>>,
    }

    impl SurfaceUser for Recorder {
        fn surface_changed(&self, surface: Option<&OutputSurface>) {
            self.seen.lock().unwrap().push(surface.map(|s| s.id));
        }
    }

    impl OutputOwner for Recorder {
        fn release(&self, index: usize, generation: u64, how: Release) -> PresentOutcome {
            self.released.lock().unwrap().push((index, generation, how));
            PresentOutcome::Shown
        }
    }

    #[test]
    fn the_slot_tells_its_users_once_per_change() {
        let slot = SurfaceSlot::new();
        assert!(!slot.is_visible() && slot.surface().is_none());
        let recorder = Arc::new(Recorder::default());
        let user: Arc<dyn SurfaceUser> = recorder.clone();
        let configured: Result<Option<u64>, ()> =
            slot.attach_with(|surface| Ok((surface.map(|s| s.id), Arc::clone(&user))));
        assert_eq!(configured, Ok(None));
        slot.set(OutputSurface::fake(1));
        assert!(slot.is_visible());
        // La misma superficie otra vez (giro de pantalla): nada que hacer.
        slot.set(OutputSurface::fake(1));
        slot.set(OutputSurface::fake(2));
        slot.clear();
        assert!(!slot.is_visible());
        slot.clear();
        assert_eq!(*recorder.seen.lock().unwrap(), vec![Some(1), Some(2), None]);
        // Un usuario que ya no existe se olvida.
        drop(user);
        drop(recorder);
        slot.set(OutputSurface::fake(3));
        assert_eq!(slot.with_surface(|s| s.map(|s| s.id)), Some(3));
        assert!(slot.same(&slot.clone()) && !slot.same(&SurfaceSlot::new()));
    }

    #[test]
    fn a_frame_is_released_exactly_once() {
        let owner = Arc::new(Recorder::default());
        let frame = SurfaceFrame::new(owner.clone(), 3, 7, (1920, 1080), 42, Instant::now());
        assert!(frame.present().shown());
        assert_eq!(frame.present(), PresentOutcome::Stale, "ya se soltó");
        drop(frame);
        let dropped = SurfaceFrame::new(owner.clone(), 4, 7, (1920, 1080), 43, Instant::now());
        drop(dropped);
        let at = SurfaceFrame::new(owner.clone(), 5, 7, (1920, 1080), 44, Instant::now());
        at.present_at(1_000);
        drop(at);
        assert_eq!(
            *owner.released.lock().unwrap(),
            vec![
                (3, 7, Release::Render),
                (4, 7, Release::Discard),
                (5, 7, Release::RenderAt(1_000))
            ]
        );
    }
}
