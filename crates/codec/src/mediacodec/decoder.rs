//! Decodificador de MediaCodec independiente de la plataforma: la máquina de estados (entrada,
//! salida, vaciado, reconfiguración, cambio de superficie), la contabilidad de búferes y marcas de
//! tiempo y la espera de la salida. La llamada real a MediaCodec va tras [`CodecOps`]
//! (`super::android` en Android; un códec falso en las pruebas).
//!
//! # Latencia de `decode`
//!
//! MediaCodec decodifica en su propio hilo: tras encolar la unidad de acceso, la imagen sale un
//! poco después (lo que tarde el hardware) o, en decodificadores que retienen fotogramas aunque
//! se configuren para baja latencia, solo cuando llega la entrada siguiente. `decode`:
//!
//! 1. Recoge sin esperar lo que ya salió (fotogramas anteriores que tardaron más que la espera).
//! 2. Encola la unidad de acceso (todos los slices) con una marca de tiempo propia y creciente.
//! 3. Espera la salida **de esa entrada** y vuelve en cuanto sale (la espera la despierta el
//!    propio códec, no se sondea). El plazo ([`WaitPolicy`]) sale de lo medido, nunca de una
//!    suposición sobre el hardware:
//!    - fotograma normal: hasta que se espera la entrada siguiente (la mediana de los intervalos
//!      medidos entre llamadas o, antes de tenerla, el intervalo de los fps de `CodecSelected`);
//!      esperar más no adelanta nada y retrasaría encolar la siguiente;
//!    - intra: además, lo que tardaron en salir los últimos intra medidos (son mayores);
//!    - decodificador que se ha visto retener fotogramas hasta la entrada siguiente
//!      ([`PIPELINE_EVIDENCE`] veces seguidas): sin esperar, porque la espera no puede sacarlo;
//!      vuelve a esperar en cuanto un fotograma sale antes de la entrada siguiente;
//!    - sin ninguna medida aún (el primer fotograma): como mucho [`OUTPUT_WAIT_LIMIT`], que no es
//!      un tiempo del hardware sino el tope de vida para un códec que no entrega nada.
//! 4. Si salieron varias imágenes, devuelve la más nueva y suelta las demás sin dibujar (como el
//!    canal «la última gana» de la sesión).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use super::info::{
    AndroidDecoderInfo, FormatParams, KEY_HEIGHT, KEY_WIDTH, format_params, grown_input_size,
};
use super::surface::{
    OutputOwner, OutputSurface, PresentOutcome, Release, SurfaceFrame, SurfaceSlot, SurfaceUser,
};
use crate::gpu::GpuImage;
use crate::{Codec, CodecError, Decoder, DecoderBackend, EncodedSlice, RawFrame, StreamHint};

/// Tope de lo que espera `decode` a un búfer de entrada o, sin ninguna medida aún, a la salida.
/// Es un límite de vida (un códec atascado no bloquea el hilo para siempre), no una estimación
/// del hardware: con medidas, la espera es la de [`WaitPolicy`]. Coincide con el periodo con el
/// que la sesión vuelve a pedir un intra cuando un fotograma no da imagen (`DECODER_RETRY` de
/// `coroom-session`): un códec que no entrega en ese tiempo se trata igual que uno sin imagen.
pub const OUTPUT_WAIT_LIMIT: Duration = Duration::from_millis(250);

/// Veces seguidas que la salida de un fotograma tiene que aparecer solo después de encolar el
/// siguiente para dar por hecho que el decodificador retiene fotogramas.
pub const PIPELINE_EVIDENCE: u32 = 3;

/// Intervalos entre llamadas de los que se toma la mediana.
const GAP_WINDOW: usize = 16;

/// Latencias de intra medidas que se recuerdan.
const KEYFRAME_WINDOW: usize = 8;

/// Entradas pendientes de salir que se recuerdan (un decodificador que descarta sin avisar no
/// hace crecer la lista).
const PENDING_LIMIT: usize = 64;

/// `AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM` (`NdkMediaCodec.h`).
pub(crate) const BUFFER_FLAG_END_OF_STREAM: u32 = 4;
/// `AMEDIACODEC_BUFFER_FLAG_DECODE_ONLY` (Android 14): búfer que no se debe mostrar.
pub(crate) const BUFFER_FLAG_DECODE_ONLY: u32 = 32;

/// Lo que devuelve [`CodecOps::dequeue_output`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputEvent {
    /// Un búfer de salida (`index`) con la marca de tiempo de su entrada.
    Frame {
        index: usize,
        pts_us: u64,
        flags: u32,
    },
    /// Cambió el formato de salida (`INFO_OUTPUT_FORMAT_CHANGED`): [`CodecOps::output_format`].
    FormatChanged,
    /// Nada en el plazo (`INFO_TRY_AGAIN_LATER`).
    TryAgain,
}

/// Por qué no se encoló una entrada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueueError {
    /// No cabe en el búfer de entrada (`capacity` bytes); el búfer sigue siendo del cliente.
    TooLarge { capacity: usize },
    /// Error de MediaCodec.
    Failed(String),
}

/// Formato de salida (`AMediaCodec_getOutputFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct OutputFormat {
    /// `width`.
    pub width: Option<i32>,
    /// `height`.
    pub height: Option<i32>,
    /// `crop-left`, `crop-top`, `crop-right`, `crop-bottom` (incluidos).
    pub crop: Option<(i32, i32, i32, i32)>,
}

impl OutputFormat {
    /// Tamaño visible: el recorte si lo hay (`right - left + 1`), si no `width × height`.
    pub fn visible_size(&self) -> Option<(u32, u32)> {
        if let Some((left, top, right, bottom)) = self.crop {
            let width = u32::try_from(i64::from(right) - i64::from(left) + 1).ok()?;
            let height = u32::try_from(i64::from(bottom) - i64::from(top) + 1).ok()?;
            if width > 0 && height > 0 {
                return Some((width, height));
            }
        }
        let width = u32::try_from(self.width?).ok()?;
        let height = u32::try_from(self.height?).ok()?;
        (width > 0 && height > 0).then_some((width, height))
    }
}

/// Las operaciones de MediaCodec que usa el decodificador. Las de `&self` se llaman a la vez desde
/// varios hilos (decodificación, presentación, interfaz); las de `&mut self`, en exclusiva.
pub(crate) trait CodecOps: Send + Sync + 'static {
    /// Un búfer de entrada libre, esperando como mucho `timeout` (`None` si no hay).
    fn dequeue_input(&self, timeout: Duration) -> Result<Option<usize>, String>;
    /// Copia `data` (los slices seguidos) en el búfer `index` y lo encola con `pts_us`.
    fn queue_input(&self, index: usize, data: &[&[u8]], pts_us: u64) -> Result<(), QueueError>;
    /// El siguiente suceso de salida, esperando como mucho `timeout`.
    fn dequeue_output(&self, timeout: Duration) -> Result<OutputEvent, String>;
    /// El formato de salida actual.
    fn output_format(&self) -> OutputFormat;
    /// Suelta el búfer de salida `index`.
    fn release_output(&self, index: usize, how: Release) -> Result<(), String>;
    /// Cambia la superficie de salida (`None`: a la de reserva fuera de pantalla).
    fn set_output(&self, surface: Option<&OutputSurface>) -> Result<(), String>;
    /// Vacía el códec (`AMediaCodec_flush`); los búferes entregados dejan de valer.
    fn flush(&mut self) -> Result<(), String>;
    /// Para, configura con `params` en `surface` (`None`: la de reserva) y arranca de nuevo.
    fn reconfigure(
        &mut self,
        params: &FormatParams,
        surface: Option<&OutputSurface>,
    ) -> Result<(), String>;
}

/// Crea el códec del componente, configurado con `params` en `surface` y arrancado.
pub(crate) type Factory<C> =
    Box<dyn Fn(&FormatParams, Option<&OutputSurface>) -> Result<C, String> + Send>;

struct CellState<C> {
    codec: Option<C>,
    /// Sube con cada vaciado, reconfiguración o cierre: un búfer entregado antes ya no es del
    /// cliente (su índice puede ser ya otro fotograma) y no se suelta.
    generation: u64,
}

/// El códec compartido entre el decodificador, los fotogramas que entregó y el sitio de la
/// superficie.
pub(crate) struct CodecCell<C> {
    state: RwLock<CellState<C>>,
    slot: SurfaceSlot,
    /// Error al cambiar de superficie desde el hilo de la interfaz: lo devuelve el siguiente
    /// `decode` y el `reset` que le sigue reconfigura.
    surface_error: Mutex<Option<String>>,
}

impl<C: CodecOps> CodecCell<C> {
    fn new(slot: SurfaceSlot) -> Self {
        Self {
            state: RwLock::new(CellState {
                codec: None,
                generation: 0,
            }),
            slot,
            surface_error: Mutex::new(None),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, CellState<C>> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, CellState<C>> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn take_surface_error(&self) -> Option<String> {
        self.surface_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn flush(&self) -> Result<(), String> {
        let mut state = self.write();
        state.generation += 1;
        match state.codec.as_mut() {
            Some(codec) => codec.flush(),
            None => Err("el decodificador está cerrado".to_owned()),
        }
    }

    /// Reconfigura con la superficie actual del sitio (con su cerrojo: nadie la cambia a la vez).
    fn reconfigure(&self, params: &FormatParams) -> Result<(), String> {
        self.slot.with_surface(|surface| {
            let mut state = self.write();
            state.generation += 1;
            match state.codec.as_mut() {
                Some(codec) => codec.reconfigure(params, surface),
                None => Err("el decodificador está cerrado".to_owned()),
            }
        })
    }

    /// Cierra el códec que hubiera (suelta la superficie) y crea otro.
    fn recreate(&self, factory: &Factory<C>, params: &FormatParams) -> Result<(), String> {
        self.slot.with_surface(|surface| {
            let mut state = self.write();
            state.generation += 1;
            // El anterior primero: una superficie solo admite un productor conectado.
            drop(state.codec.take());
            state.codec = Some(factory(params, surface)?);
            Ok(())
        })
    }

    fn close(&self) {
        let mut state = self.write();
        state.generation += 1;
        drop(state.codec.take());
    }
}

impl<C: CodecOps> OutputOwner for CodecCell<C> {
    fn release(&self, index: usize, generation: u64, how: Release) -> PresentOutcome {
        let state = self.read();
        let Some(codec) = state.codec.as_ref() else {
            return PresentOutcome::Stale;
        };
        if state.generation != generation {
            return PresentOutcome::Stale;
        }
        // Sin superficie en pantalla, a la de reserva no se dibuja nada.
        let hidden = how != Release::Discard && !self.slot.is_visible();
        let how = if hidden { Release::Discard } else { how };
        match codec.release_output(index, how) {
            Ok(()) if hidden => PresentOutcome::Hidden,
            Ok(()) if how == Release::Discard => PresentOutcome::Discarded,
            Ok(()) => PresentOutcome::Shown,
            Err(err) => PresentOutcome::Failed(err),
        }
    }
}

impl<C: CodecOps> SurfaceUser for CodecCell<C> {
    fn surface_changed(&self, surface: Option<&OutputSurface>) {
        let state = self.read();
        if let Some(codec) = state.codec.as_ref()
            && let Err(err) = codec.set_output(surface)
        {
            *self
                .surface_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(err);
        }
    }
}

/// Ventana de las últimas `N` duraciones.
#[derive(Debug, Clone, Default)]
struct Recent {
    values: VecDeque<Duration>,
    limit: usize,
}

impl Recent {
    fn new(limit: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(limit),
            limit,
        }
    }

    fn push(&mut self, value: Duration) {
        if self.values.len() == self.limit {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    fn median(&self) -> Option<Duration> {
        let mut sorted: Vec<Duration> = self.values.iter().copied().collect();
        sorted.sort_unstable();
        sorted.get(sorted.len() / 2).copied()
    }

    fn max(&self) -> Option<Duration> {
        self.values.iter().copied().max()
    }
}

/// En qué momento de `decode` salió un fotograma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Al empezar, antes de encolar la entrada nueva: salió entre dos llamadas.
    BeforeQueue,
    /// Tras encolar la entrada nueva.
    AfterQueue,
}

/// Cuánto espera `decode` la salida (ver el módulo).
#[derive(Debug, Clone)]
pub(crate) struct WaitPolicy {
    last_arrival: Option<Instant>,
    gaps: Recent,
    /// Intervalo de los fps del flujo (`CodecSelected`), hasta tener intervalos medidos.
    hint: Option<Duration>,
    keyframe_latency: Recent,
    late_in_a_row: u32,
    pipelined: bool,
}

impl WaitPolicy {
    pub(crate) fn new(hint: Option<StreamHint>) -> Self {
        Self {
            last_arrival: None,
            gaps: Recent::new(GAP_WINDOW),
            hint: hint
                .filter(|hint| hint.fps > 0)
                .map(|hint| Duration::from_secs(1) / hint.fps),
            keyframe_latency: Recent::new(KEYFRAME_WINDOW),
            late_in_a_row: 0,
            pipelined: false,
        }
    }

    /// Llegó una unidad de acceso en `at`.
    pub(crate) fn note_arrival(&mut self, at: Instant) {
        if let Some(last) = self.last_arrival {
            let gap = at.saturating_duration_since(last);
            if !gap.is_zero() {
                self.gaps.push(gap);
            }
        }
        self.last_arrival = Some(at);
    }

    /// Intervalo esperado entre unidades de acceso: lo medido o, sin medidas, lo anunciado.
    pub(crate) fn interval(&self) -> Option<Duration> {
        self.gaps.median().or(self.hint)
    }

    /// Si se ha visto que retiene fotogramas hasta la entrada siguiente.
    pub(crate) fn pipelined(&self) -> bool {
        self.pipelined
    }

    /// Cuánto esperar la salida de la entrada que llegó en `arrival`, siendo ahora `now`.
    pub(crate) fn output_wait(&self, keyframe: bool, arrival: Instant, now: Instant) -> Duration {
        if self.pipelined {
            return Duration::ZERO;
        }
        let until = |span: Duration| (arrival + span).saturating_duration_since(now);
        let next_due = self.interval().map(until);
        let wait = if keyframe {
            match (self.keyframe_latency.max().map(until), next_due) {
                (Some(measured), next) => measured.max(next.unwrap_or_default()),
                (None, _) => OUTPUT_WAIT_LIMIT,
            }
        } else {
            next_due.unwrap_or(OUTPUT_WAIT_LIMIT)
        };
        wait.min(OUTPUT_WAIT_LIMIT)
    }

    /// Salió la imagen de `pending` en `now`, en la fase `phase`; `own` si es la de la entrada que
    /// se acababa de encolar.
    pub(crate) fn note_output(&mut self, pending: &Pending, phase: Phase, own: bool, now: Instant) {
        if pending.keyframe {
            self.keyframe_latency
                .push(now.saturating_duration_since(pending.queued_at));
        }
        match (phase, own) {
            (Phase::AfterQueue, false) => {
                // Solo salió tras encolar la siguiente.
                self.late_in_a_row += 1;
                if self.late_in_a_row >= PIPELINE_EVIDENCE {
                    self.pipelined = true;
                }
            }
            // Salió sola (en su llamada o entre dos): esperar sirve.
            _ => {
                self.late_in_a_row = 0;
                self.pipelined = false;
            }
        }
    }
}

/// Una entrada encolada cuya imagen aún no ha salido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pending {
    pub pts_us: u64,
    pub frame_id: u64,
    pub queued_at: Instant,
    pub keyframe: bool,
}

/// Entradas pendientes, en el orden en que se encolaron (sin B-frames, el de salida).
#[derive(Debug, Default)]
pub(crate) struct PtsBook {
    pending: VecDeque<Pending>,
}

impl PtsBook {
    pub(crate) fn push(&mut self, pending: Pending) {
        if self.pending.len() == PENDING_LIMIT {
            self.pending.pop_front();
        }
        self.pending.push_back(pending);
    }

    /// La entrada de `pts_us` (y olvida las anteriores: el decodificador las descartó).
    pub(crate) fn take(&mut self, pts_us: u64) -> Option<Pending> {
        while let Some(front) = self.pending.front() {
            if front.pts_us < pts_us {
                self.pending.pop_front();
            } else if front.pts_us == pts_us {
                return self.pending.pop_front();
            } else {
                return None;
            }
        }
        None
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }
}

/// Marca de tiempo de una entrada que llega en `arrival`: microsegundos desde `origin`,
/// siempre mayor que la anterior (hay decodificadores que reordenan las marcas que reciben; con
/// marcas crecientes y sin B-frames, el orden de salida es el de entrada).
pub(crate) fn next_pts(origin: Instant, arrival: Instant, last: Option<u64>) -> u64 {
    let elapsed =
        u64::try_from(arrival.saturating_duration_since(origin).as_micros()).unwrap_or(u64::MAX);
    match last {
        Some(last) => elapsed.max(last.saturating_add(1)),
        None => elapsed,
    }
}

/// Decodificador de MediaCodec sobre las operaciones `C`.
pub(crate) struct MediaCodecDecoder<C: CodecOps> {
    codec: Codec,
    info: AndroidDecoderInfo,
    hint: Option<StreamHint>,
    cell: Arc<CodecCell<C>>,
    factory: Factory<C>,
    params: FormatParams,
    /// Bytes de la mayor unidad de acceso que no cupo (para `max-input-size`).
    max_input: Option<usize>,
    /// El siguiente `reset` reconfigura en vez de vaciar.
    reconfigure: bool,
    book: PtsBook,
    wait: WaitPolicy,
    format: OutputFormat,
    origin: Instant,
    last_pts: Option<u64>,
}

impl<C: CodecOps> MediaCodecDecoder<C> {
    /// Crea el códec de `info` con `factory` en la superficie de `slot` (o en la de reserva).
    pub(crate) fn open(
        codec: Codec,
        info: AndroidDecoderInfo,
        hint: Option<StreamHint>,
        slot: &SurfaceSlot,
        factory: Factory<C>,
    ) -> Result<Self, CodecError> {
        let params = format_params(&info, codec, hint, None);
        let cell = Arc::new(CodecCell::new(slot.clone()));
        slot.attach_with(|surface| {
            let created = factory(&params, surface).map_err(|err| {
                CodecError::Backend(format!("no se pudo abrir {}: {err}", info.name))
            })?;
            cell.write().codec = Some(created);
            let user: Arc<dyn SurfaceUser> = cell.clone();
            Ok(((), user))
        })?;
        Ok(Self {
            codec,
            info,
            hint,
            cell,
            factory,
            params,
            max_input: None,
            reconfigure: false,
            book: PtsBook::default(),
            wait: WaitPolicy::new(hint),
            format: OutputFormat::default(),
            origin: Instant::now(),
            last_pts: None,
        })
    }

    /// Nombre del componente.
    #[cfg(test)]
    pub(crate) fn component(&self) -> &str {
        &self.info.name
    }

    /// Tamaño de las imágenes: el visible del formato de salida o, antes de conocerlo, el
    /// configurado.
    fn frame_size(&self) -> (u32, u32) {
        self.format
            .visible_size()
            .or_else(|| self.params.size())
            .unwrap_or((0, 0))
    }

    /// Recoge lo que salga en `wait` (devuelve en cuanto sale la imagen de `own`, si se da). La
    /// imagen que queda en `latest` es la más nueva; las que reemplaza vuelven al códec sin
    /// dibujarse (directamente: aquí ya se tiene el cerrojo del códec).
    fn drain(
        &mut self,
        codec: &C,
        wait: Duration,
        phase: Phase,
        own: Option<u64>,
        latest: &mut Option<Ready>,
    ) -> Result<(), CodecError> {
        let deadline = Instant::now() + wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match codec
                .dequeue_output(remaining)
                .map_err(CodecError::Backend)?
            {
                OutputEvent::Frame {
                    index,
                    pts_us,
                    flags,
                } => {
                    let now = Instant::now();
                    let shown = flags & (BUFFER_FLAG_DECODE_ONLY | BUFFER_FLAG_END_OF_STREAM) == 0;
                    // `None`: de antes de un vaciado, o una marca que no es nuestra.
                    let pending = if shown { self.book.take(pts_us) } else { None };
                    let Some(pending) = pending else {
                        let _ = codec.release_output(index, Release::Discard);
                        continue;
                    };
                    let is_own = own == Some(pts_us);
                    self.wait.note_output(&pending, phase, is_own, now);
                    let ready = Ready {
                        index,
                        pts_us,
                        frame_id: pending.frame_id,
                        decoded_at: now,
                    };
                    if let Some(older) = latest.replace(ready) {
                        let _ = codec.release_output(older.index, Release::Discard);
                    }
                    if is_own {
                        return Ok(());
                    }
                }
                OutputEvent::FormatChanged => self.format = codec.output_format(),
                OutputEvent::TryAgain => return Ok(()),
            }
        }
    }

    /// La entrada (paso 2 de `decode`): un búfer libre, esperando como mucho el tope de vida si
    /// el códec no tiene ninguno (mientras, se recoge lo que haya salido), y encolarla.
    fn queue(
        &mut self,
        codec: &C,
        data: &[&[u8]],
        arrival: Instant,
        latest: &mut Option<Ready>,
    ) -> Result<u64, CodecError> {
        let index = match codec
            .dequeue_input(Duration::ZERO)
            .map_err(CodecError::Backend)?
        {
            Some(index) => index,
            None => {
                self.drain(codec, Duration::ZERO, Phase::BeforeQueue, None, latest)?;
                let remaining =
                    (arrival + OUTPUT_WAIT_LIMIT).saturating_duration_since(Instant::now());
                codec
                    .dequeue_input(remaining)
                    .map_err(CodecError::Backend)?
                    .ok_or_else(|| {
                        CodecError::Backend(format!(
                            "{} no acepta más datos (sin búfer de entrada libre)",
                            self.info.name
                        ))
                    })?
            }
        };
        let pts_us = next_pts(self.origin, arrival, self.last_pts);
        match codec.queue_input(index, data, pts_us) {
            Ok(()) => {
                self.last_pts = Some(pts_us);
                Ok(pts_us)
            }
            Err(QueueError::TooLarge { capacity }) => {
                // Se reconfigura con un búfer mayor en el `reset` que sigue al error.
                let total: usize = data.iter().map(|part| part.len()).sum();
                self.max_input = Some(grown_input_size(total).max(self.max_input.unwrap_or(0)));
                self.reconfigure = true;
                Err(CodecError::Backend(format!(
                    "la unidad de acceso ({total} bytes) no cabe en el búfer de entrada \
                     ({capacity} bytes)"
                )))
            }
            Err(QueueError::Failed(err)) => Err(CodecError::Backend(err)),
        }
    }

    /// Pasos 1 a 3 de `decode` con el cerrojo del códec tomado.
    fn decode_locked(
        &mut self,
        codec: &C,
        data: &[&[u8]],
        (frame_id, keyframe): (u64, bool),
        arrival: Instant,
        latest: &mut Option<Ready>,
    ) -> Result<(), CodecError> {
        // 1. Lo que ya salió entre dos llamadas.
        self.drain(codec, Duration::ZERO, Phase::BeforeQueue, None, latest)?;
        // 2. La entrada.
        let pts_us = self.queue(codec, data, arrival, latest)?;
        let queued_at = Instant::now();
        self.book.push(Pending {
            pts_us,
            frame_id,
            queued_at,
            keyframe,
        });
        // 3. Su salida, sin esperar más de lo que dice lo medido.
        let wait = self.wait.output_wait(keyframe, arrival, queued_at);
        self.drain(codec, wait, Phase::AfterQueue, Some(pts_us), latest)
    }
}

/// Un búfer de salida ya recogido, aún sin [`SurfaceFrame`].
#[derive(Debug, Clone, Copy)]
struct Ready {
    index: usize,
    pts_us: u64,
    frame_id: u64,
    decoded_at: Instant,
}

impl<C: CodecOps> Decoder for MediaCodecDecoder<C> {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn backend(&self) -> DecoderBackend {
        DecoderBackend::MediaCodec
    }

    fn decode(&mut self, slices: &[EncodedSlice]) -> Result<Option<RawFrame>, CodecError> {
        let Some(first) = slices.first() else {
            return Ok(None);
        };
        let arrival = Instant::now();
        self.wait.note_arrival(arrival);
        if let Some(err) = self.cell.take_surface_error() {
            self.reconfigure = true;
            return Err(CodecError::Backend(format!(
                "no se pudo cambiar la superficie: {err}"
            )));
        }
        let frame_id = first.frame_id;
        let keyframe = slices.iter().any(|slice| slice.keyframe);
        let data: Vec<&[u8]> = slices.iter().map(|slice| slice.data.as_slice()).collect();
        if data.iter().all(|part| part.is_empty()) {
            return Ok(None);
        }
        let cell = Arc::clone(&self.cell);
        let state = cell.read();
        let Some(codec) = state.codec.as_ref() else {
            return Err(CodecError::Backend(
                "el decodificador está cerrado".to_owned(),
            ));
        };
        let generation = state.generation;
        let mut latest: Option<Ready> = None;
        let result = self.decode_locked(codec, &data, (frame_id, keyframe), arrival, &mut latest);
        if result.is_err()
            && let Some(ready) = latest.take()
        {
            let _ = codec.release_output(ready.index, Release::Discard);
        }
        drop(state);
        result?;
        let size = self.frame_size();
        Ok(latest.map(|ready| {
            let owner: Arc<dyn OutputOwner> = cell;
            let frame = SurfaceFrame::new(
                owner,
                ready.index,
                generation,
                size,
                ready.pts_us,
                ready.decoded_at,
            );
            RawFrame::gpu(ready.frame_id, size, GpuImage::MediaCodec(Arc::new(frame)))
        }))
    }

    /// Vacía el códec (`AMediaCodec_flush`): descarta las referencias sin recrearlo. Si el vaciado
    /// falla (el códec quedó en error) o hay que cambiar algo de la configuración (un búfer de
    /// entrada mayor, una superficie que no se pudo cambiar), lo reconfigura; si tampoco, lo crea
    /// de nuevo.
    fn reset(&mut self) {
        self.book.clear();
        self.last_pts = None;
        self.origin = Instant::now();
        let reconfigure = std::mem::take(&mut self.reconfigure);
        if reconfigure {
            self.params = format_params(&self.info, self.codec, self.hint, self.max_input);
        }
        let flushed = !reconfigure && self.cell.flush().is_ok();
        if flushed {
            return;
        }
        if self.cell.reconfigure(&self.params).is_ok() {
            self.format = OutputFormat::default();
            return;
        }
        self.format = OutputFormat::default();
        if self.cell.recreate(&self.factory, &self.params).is_err() {
            // El siguiente `decode` dirá que está cerrado y la sesión pasará a otro.
            self.cell.close();
        }
    }

    fn cpu_copies(&self) -> u32 {
        0
    }
}

impl<C: CodecOps> Drop for MediaCodecDecoder<C> {
    fn drop(&mut self) {
        // Los fotogramas que sigan vivos ya no tocan el códec (otra generación).
        self.cell.close();
    }
}

impl<C: CodecOps> std::fmt::Debug for MediaCodecDecoder<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaCodecDecoder")
            .field("codec", &self.codec)
            .field("component", &self.info.name)
            .field(
                "size",
                &(self.params.i32(KEY_WIDTH), self.params.i32(KEY_HEIGHT)),
            )
            .field("pipelined", &self.wait.pipelined())
            .finish_non_exhaustive()
    }
}

// Con superficies y códecs falsos: solo en el equipo de desarrollo.
#[cfg(test)]
#[cfg(not(target_os = "android"))]
mod tests {
    use super::*;
    use crate::mediacodec::info::tests::info;
    use crate::mediacodec::info::{KEY_MAX_INPUT_SIZE, MIME_HEVC};

    /// Lo que hace el códec falso al recibir una entrada.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Mode {
        /// La imagen sale en cuanto se encola.
        Prompt,
        /// La imagen de una entrada sale al encolar la siguiente.
        HoldsOne,
    }

    #[derive(Debug, Default)]
    struct FakeState {
        free_inputs: VecDeque<usize>,
        outputs: VecDeque<OutputEvent>,
        held: Option<(usize, u64)>,
        next_output: usize,
        queued: Vec<(usize, usize, u64)>,
        released: Vec<(usize, Release)>,
        surfaces: Vec<Option<OutputSurface>>,
        waits: Vec<Duration>,
        flushes: u32,
        reconfigures: Vec<FormatParams>,
        capacity: usize,
        fail_dequeue: bool,
        fail_flush: bool,
    }

    /// Códec falso: nunca duerme; anota los plazos que se le piden.
    #[derive(Debug, Clone)]
    struct Fake {
        mode: Mode,
        state: Arc<Mutex<FakeState>>,
    }

    impl Fake {
        fn new(mode: Mode) -> Self {
            let state = FakeState {
                free_inputs: (0..4).collect(),
                capacity: 1024,
                ..FakeState::default()
            };
            Self {
                mode,
                state: Arc::new(Mutex::new(state)),
            }
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state.lock().unwrap()
        }

        fn emit(state: &mut FakeState, pts_us: u64) {
            let index = state.next_output;
            state.next_output += 1;
            state.outputs.push_back(OutputEvent::Frame {
                index,
                pts_us,
                flags: 0,
            });
        }
    }

    impl CodecOps for Fake {
        fn dequeue_input(&self, _timeout: Duration) -> Result<Option<usize>, String> {
            Ok(self.lock().free_inputs.pop_front())
        }

        fn queue_input(&self, index: usize, data: &[&[u8]], pts_us: u64) -> Result<(), QueueError> {
            let mut state = self.lock();
            let len: usize = data.iter().map(|part| part.len()).sum();
            if len > state.capacity {
                state.free_inputs.push_front(index);
                return Err(QueueError::TooLarge {
                    capacity: state.capacity,
                });
            }
            state.queued.push((index, len, pts_us));
            state.free_inputs.push_back(index);
            match self.mode {
                Mode::Prompt => Self::emit(&mut state, pts_us),
                Mode::HoldsOne => {
                    if let Some((_, held)) = state.held.take() {
                        Self::emit(&mut state, held);
                    }
                    state.held = Some((index, pts_us));
                }
            }
            Ok(())
        }

        fn dequeue_output(&self, timeout: Duration) -> Result<OutputEvent, String> {
            let mut state = self.lock();
            if state.fail_dequeue {
                return Err("AMEDIA_ERROR_UNKNOWN".to_owned());
            }
            state.waits.push(timeout);
            Ok(state.outputs.pop_front().unwrap_or(OutputEvent::TryAgain))
        }

        fn output_format(&self) -> OutputFormat {
            OutputFormat {
                width: Some(1920),
                height: Some(1088),
                crop: Some((0, 0, 1919, 1079)),
            }
        }

        fn release_output(&self, index: usize, how: Release) -> Result<(), String> {
            self.lock().released.push((index, how));
            Ok(())
        }

        fn set_output(&self, surface: Option<&OutputSurface>) -> Result<(), String> {
            self.lock().surfaces.push(surface.cloned());
            Ok(())
        }

        fn flush(&mut self) -> Result<(), String> {
            let mut state = self.lock();
            state.flushes += 1;
            state.outputs.clear();
            state.held = None;
            if state.fail_flush {
                return Err("estado de error".to_owned());
            }
            Ok(())
        }

        fn reconfigure(
            &mut self,
            params: &FormatParams,
            surface: Option<&OutputSurface>,
        ) -> Result<(), String> {
            let mut state = self.lock();
            state.reconfigures.push(params.clone());
            state.surfaces.push(surface.cloned());
            state.outputs.clear();
            state.held = None;
            state.fail_dequeue = false;
            state.fail_flush = false;
            if let Some(size) = params.i32(KEY_MAX_INPUT_SIZE) {
                state.capacity = usize::try_from(size).unwrap();
            }
            Ok(())
        }
    }

    fn open(fake: &Fake, slot: &SurfaceSlot, hint: Option<StreamHint>) -> MediaCodecDecoder<Fake> {
        let shared = fake.clone();
        let factory: Factory<Fake> = Box::new(move |_params, surface| {
            shared.lock().surfaces.push(surface.cloned());
            Ok(shared.clone())
        });
        MediaCodecDecoder::open(
            Codec::Hevc,
            info("c2.example.hevc.decoder", MIME_HEVC),
            hint,
            slot,
            factory,
        )
        .expect("abre")
    }

    fn slice(frame_id: u64, keyframe: bool, len: usize) -> EncodedSlice {
        EncodedSlice {
            frame_id,
            index: 0,
            count: 1,
            keyframe,
            data: vec![0; len],
        }
    }

    fn surface_frame(frame: &RawFrame) -> &SurfaceFrame {
        match frame.gpu.as_ref() {
            Some(GpuImage::MediaCodec(image)) => image,
            other => panic!("se esperaba un fotograma de MediaCodec: {other:?}"),
        }
    }

    #[test]
    fn output_formats_report_the_visible_size() {
        let format = OutputFormat {
            width: Some(1920),
            height: Some(1088),
            crop: Some((0, 0, 1919, 1079)),
        };
        assert_eq!(format.visible_size(), Some((1920, 1080)));
        let uncropped = OutputFormat {
            crop: None,
            ..format
        };
        assert_eq!(uncropped.visible_size(), Some((1920, 1088)));
        let broken = OutputFormat {
            width: None,
            height: None,
            crop: Some((10, 10, 0, 0)),
        };
        assert_eq!(broken.visible_size(), None);
    }

    #[test]
    fn timestamps_always_grow() {
        let origin = Instant::now();
        assert_eq!(next_pts(origin, origin, None), 0);
        assert_eq!(next_pts(origin, origin, Some(0)), 1);
        let later = origin + Duration::from_millis(16);
        assert_eq!(next_pts(origin, later, Some(5)), 16_000);
        assert_eq!(next_pts(origin, origin, Some(u64::MAX)), u64::MAX);
    }

    #[test]
    fn the_book_forgets_what_the_decoder_dropped() {
        let at = Instant::now();
        let mut book = PtsBook::default();
        for pts in [10, 20, 30] {
            book.push(Pending {
                pts_us: pts,
                frame_id: pts / 10,
                queued_at: at,
                keyframe: false,
            });
        }
        assert_eq!(
            book.take(20).map(|p| p.frame_id),
            Some(2),
            "el 10 se perdió"
        );
        assert_eq!(book.take(25), None);
        assert_eq!(book.len(), 1);
        for pts in 0..(PENDING_LIMIT as u64 + 10) {
            book.push(Pending {
                pts_us: 100 + pts,
                frame_id: pts,
                queued_at: at,
                keyframe: false,
            });
        }
        assert_eq!(book.len(), PENDING_LIMIT);
    }

    /// La espera sale de lo medido: hasta la entrada siguiente, más lo que tardan los intra, y
    /// nada si el decodificador retiene fotogramas.
    #[test]
    fn the_wait_follows_what_was_measured() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        let mut policy = WaitPolicy::new(None);
        // Sin medidas: el tope de vida.
        assert_eq!(policy.output_wait(false, start, start), OUTPUT_WAIT_LIMIT);
        assert_eq!(policy.output_wait(true, start, start), OUTPUT_WAIT_LIMIT);
        // Con los fps anunciados, hasta la entrada siguiente (60 fps: 16,6 ms).
        let hinted = WaitPolicy::new(Some(StreamHint {
            width: 1920,
            height: 1080,
            fps: 60,
        }));
        assert_eq!(
            hinted.output_wait(false, start, start + ms(2)),
            Duration::from_secs(1) / 60 - ms(2)
        );
        // Medido: la mediana de los intervalos manda sobre lo anunciado.
        let mut at = start;
        for _ in 0..5 {
            policy.note_arrival(at);
            at += ms(8);
        }
        assert_eq!(policy.interval(), Some(ms(8)));
        let arrival = at;
        assert_eq!(policy.output_wait(false, arrival, arrival + ms(1)), ms(7));
        assert_eq!(
            policy.output_wait(false, arrival, arrival + ms(9)),
            Duration::ZERO,
            "ya toca la siguiente"
        );
        // Un intra medido de 20 ms: se espera eso aunque pase la siguiente.
        let key = Pending {
            pts_us: 1,
            frame_id: 1,
            queued_at: arrival,
            keyframe: true,
        };
        policy.note_output(&key, Phase::AfterQueue, true, arrival + ms(20));
        assert_eq!(policy.output_wait(true, arrival, arrival + ms(1)), ms(19));
        assert_eq!(policy.output_wait(false, arrival, arrival + ms(1)), ms(7));
        // Tres seguidas que solo salen tras encolar la siguiente: no se espera.
        let normal = Pending {
            keyframe: false,
            ..key
        };
        for _ in 0..PIPELINE_EVIDENCE {
            assert!(!policy.pipelined());
            policy.note_output(&normal, Phase::AfterQueue, false, arrival);
        }
        assert!(policy.pipelined());
        assert_eq!(policy.output_wait(true, arrival, arrival), Duration::ZERO);
        // Una que sale entre dos llamadas: esperar vuelve a servir.
        policy.note_output(&normal, Phase::BeforeQueue, false, arrival);
        assert!(!policy.pipelined());
    }

    #[test]
    fn a_prompt_decoder_returns_each_frame_in_its_own_call() {
        let fake = Fake::new(Mode::Prompt);
        let slot = SurfaceSlot::new();
        slot.set(OutputSurface::fake(1));
        let mut decoder = open(&fake, &slot, None);
        assert_eq!(decoder.component(), "c2.example.hevc.decoder");
        assert_eq!(decoder.backend(), DecoderBackend::MediaCodec);
        assert_eq!(decoder.cpu_copies(), 0);
        for id in 0..3 {
            let frame = decoder
                .decode(&[slice(id, id == 0, 100)])
                .unwrap()
                .expect("imagen");
            assert_eq!(frame.frame_id, id);
            assert_eq!(frame.format, crate::PixelFormat::Gpu);
            assert_eq!(frame.data.len(), 0);
            assert!(surface_frame(&frame).present().shown());
        }
        let state = fake.lock();
        assert_eq!(state.queued.len(), 3);
        assert_eq!(
            state.released,
            vec![
                (0, Release::Render),
                (1, Release::Render),
                (2, Release::Render)
            ]
        );
        // Configurado con la superficie del sitio.
        assert_eq!(state.surfaces, vec![Some(OutputSurface::fake(1))]);
    }

    #[test]
    fn a_decoder_that_holds_a_frame_is_detected_and_not_waited_for() {
        let fake = Fake::new(Mode::HoldsOne);
        let slot = SurfaceSlot::new();
        slot.set(OutputSurface::fake(1));
        let mut decoder = open(
            &fake,
            &slot,
            Some(StreamHint {
                width: 1280,
                height: 720,
                fps: 60,
            }),
        );
        assert!(decoder.decode(&[slice(0, true, 10)]).unwrap().is_none());
        for id in 1..=(PIPELINE_EVIDENCE as u64 + 1) {
            let frame = decoder.decode(&[slice(id, false, 10)]).unwrap().unwrap();
            assert_eq!(frame.frame_id, id - 1, "sale la anterior");
        }
        assert!(decoder.wait.pipelined());
        fake.lock().waits.clear();
        decoder.decode(&[slice(10, false, 10)]).unwrap();
        assert!(
            fake.lock().waits.iter().all(|wait| wait.is_zero()),
            "{:?}",
            fake.lock().waits
        );
    }

    /// Si salen varias, se devuelve la más nueva y las demás vuelven al códec sin dibujarse; un
    /// fotograma que la presentación deja caer también.
    #[test]
    fn older_and_dropped_frames_are_released_without_rendering() {
        let fake = Fake::new(Mode::Prompt);
        let slot = SurfaceSlot::new();
        slot.set(OutputSurface::fake(1));
        let mut decoder = open(&fake, &slot, None);
        let first = decoder.decode(&[slice(0, true, 10)]).unwrap().unwrap();
        // Dos imágenes de entradas que no son de esta llamada, ya disponibles.
        let injected = decoder.last_pts.unwrap() + 1;
        decoder.last_pts = Some(injected);
        decoder.book.push(Pending {
            pts_us: injected,
            frame_id: 100,
            queued_at: Instant::now(),
            keyframe: false,
        });
        {
            let mut state = fake.lock();
            Fake::emit(&mut state, injected);
        }
        let newest = decoder.decode(&[slice(1, false, 10)]).unwrap().unwrap();
        assert_eq!(newest.frame_id, 1);
        drop(first);
        drop(newest);
        let state = fake.lock();
        assert_eq!(
            state.released,
            vec![
                (1, Release::Discard),
                (0, Release::Discard),
                (2, Release::Discard)
            ]
        );
    }

    /// Sin superficie, a la de reserva y sin dibujar; con otra, se cambia sin recrear el códec.
    #[test]
    fn surface_changes_reach_the_codec_without_recreating_it() {
        let fake = Fake::new(Mode::Prompt);
        let slot = SurfaceSlot::new();
        let mut decoder = open(&fake, &slot, None);
        let hidden = decoder.decode(&[slice(0, true, 10)]).unwrap().unwrap();
        assert_eq!(surface_frame(&hidden).present(), PresentOutcome::Hidden);
        slot.set(OutputSurface::fake(7));
        let shown = decoder.decode(&[slice(1, false, 10)]).unwrap().unwrap();
        assert!(surface_frame(&shown).present().shown());
        slot.clear();
        let state = fake.lock();
        assert_eq!(
            state.surfaces,
            vec![None, Some(OutputSurface::fake(7)), None],
            "abierto en la de reserva, luego la 7, luego la de reserva"
        );
        assert_eq!(
            state.released,
            vec![(0, Release::Discard), (1, Release::Render)]
        );
    }

    /// Tras un vaciado los índices entregados ya no son nuestros: soltarlos no toca el códec.
    #[test]
    fn frames_from_before_a_flush_never_touch_the_codec() {
        let fake = Fake::new(Mode::Prompt);
        let slot = SurfaceSlot::new();
        slot.set(OutputSurface::fake(1));
        let mut decoder = open(&fake, &slot, None);
        let stale = decoder.decode(&[slice(0, true, 10)]).unwrap().unwrap();
        decoder.reset();
        assert_eq!(fake.lock().flushes, 1);
        assert_eq!(surface_frame(&stale).present(), PresentOutcome::Stale);
        drop(decoder);
        let state = fake.lock();
        assert!(state.released.is_empty(), "{:?}", state.released);
    }

    /// Un error del códec llega como `CodecError` (la sesión pide un intra) y el `reset` lo
    /// recupera: si el vaciado falla, se reconfigura.
    #[test]
    fn codec_errors_surface_and_reset_recovers() {
        let fake = Fake::new(Mode::Prompt);
        let slot = SurfaceSlot::new();
        slot.set(OutputSurface::fake(1));
        let mut decoder = open(&fake, &slot, None);
        {
            let mut state = fake.lock();
            state.fail_dequeue = true;
            state.fail_flush = true;
        }
        assert!(matches!(
            decoder.decode(&[slice(0, true, 10)]),
            Err(CodecError::Backend(_))
        ));
        decoder.reset();
        assert_eq!(fake.lock().reconfigures.len(), 1);
        assert!(decoder.decode(&[slice(1, true, 10)]).unwrap().is_some());
    }

    /// Una unidad de acceso que no cabe: error, y el `reset` reconfigura con un búfer mayor.
    #[test]
    fn an_access_unit_too_large_grows_the_input_buffer() {
        let fake = Fake::new(Mode::Prompt);
        let slot = SurfaceSlot::new();
        slot.set(OutputSurface::fake(1));
        let mut decoder = open(&fake, &slot, None);
        let big = [slice(0, true, 600), slice(0, true, 600)];
        assert!(decoder.decode(&big).is_err());
        decoder.reset();
        let params = fake
            .lock()
            .reconfigures
            .last()
            .cloned()
            .expect("reconfigura");
        assert_eq!(params.i32(KEY_MAX_INPUT_SIZE), Some(2048));
        assert!(decoder.decode(&big).unwrap().is_some());
        assert_eq!(fake.lock().queued.last().map(|q| q.1), Some(1200));
    }

    #[test]
    fn empty_input_gives_no_frame() {
        let fake = Fake::new(Mode::Prompt);
        let slot = SurfaceSlot::new();
        let mut decoder = open(&fake, &slot, None);
        assert_eq!(decoder.decode(&[]), Ok(None));
        assert_eq!(decoder.decode(&[slice(0, true, 0)]), Ok(None));
        assert!(fake.lock().queued.is_empty());
    }
}
