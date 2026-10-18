//! Dentro de una sala: la sesión del invitado con MediaCodec en la `Surface` de la vista, AAudio,
//! y la entrada del teléfono (tacto como ratón, teclado y mandos) hacia el anfitrión.
//!
//! Hilos: el de los eventos de la sesión (hasta que termina) y el de los mandos, que cada
//! [`IDLE_POLL`] sigue la ranura que da el anfitrión, le pasa su vibración a la aplicación y
//! actualiza el tamaño del fotograma del anfitrión para el tacto. Todo lo demás llega por JNI en
//! el hilo de la interfaz y vuelve enseguida.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Instant;

use coroom_session::Session as _;
use coroom_session::account::{self, AccountStorage, ServerOptions};
use coroom_session::audio::AudioFormat;
use coroom_session::capture::{CursorShape, CursorState};
use coroom_session::codec::mediacodec::{AndroidDecoderInfo, SurfaceSlot};
use coroom_session::codec::{DecoderSet, GpuImage};
use coroom_session::input::android::{AndroidMotors, AndroidPads, PadAxes, evdev_from_android};
use coroom_session::input::capture::Allowed;
use coroom_session::input::gamepad::{IDLE_POLL, PadChoice, PadKey, PadLink};
use coroom_session::input::touch::{
    TouchAction, TouchConfig, TouchMapper, TouchMode, TouchPointer,
};
use coroom_session::input::{InputEvent, Rumble};
use coroom_session::latency::LatencyLedger;
use coroom_session::loopback::{CursorSink, DecodedFrame, FrameSink, SinkError};
use coroom_session::remote::{
    AudioControl, GuestAudio, GuestInput, RemoteConfig, RemoteSession, SessionEvent, TalkMode,
    VoiceInput,
};
use coroom_transport::quic::Path;

use crate::events::{self, UiEvent};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Formas de cursor que se guardan (las más viejas se olvidan).
const CURSOR_SHAPES: usize = 16;

/// Lo que la aplicación pide al entrar.
#[derive(Debug, Clone)]
pub struct JoinRequest {
    /// Código de la sala.
    pub code: String,
    /// Servidor (`None`: el último).
    pub server: Option<String>,
    /// Con micrófono (la aplicación ya tiene el permiso).
    pub mic: bool,
    /// Entrar con el micrófono silenciado.
    pub mic_muted: bool,
    /// Volumen del juego en tanto por ciento (100 = tal cual).
    pub game_volume: u32,
}

/// Donde va un evento para la interfaz.
pub type Emit = Arc<dyn Fn(UiEvent) + Send + Sync>;

/// El cursor del anfitrión, para que la aplicación lo dibuje sobre el vídeo.
#[derive(Default)]
pub struct CursorSlot {
    state: Mutex<Option<CursorState>>,
    seq: AtomicU64,
    shapes: Mutex<Vec<Arc<CursorShape>>>,
}

impl CursorSlot {
    fn update(&self, cursor: CursorState, shape: Option<Arc<CursorShape>>) {
        if let Some(shape) = shape {
            let mut shapes = lock(&self.shapes);
            if !shapes.iter().any(|known| known.shape_id == shape.shape_id) {
                if shapes.len() >= CURSOR_SHAPES {
                    shapes.remove(0);
                }
                shapes.push(shape);
            }
        }
        let mut state = lock(&self.state);
        if state.as_ref() != Some(&cursor) {
            *state = Some(cursor);
            self.seq.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// El cursor actual y su número de cambio.
    pub fn current(&self) -> (u64, Option<CursorState>) {
        (self.seq.load(Ordering::Acquire), *lock(&self.state))
    }

    /// La forma `shape_id`, si llegó.
    pub fn shape(&self, shape_id: u64) -> Option<Arc<CursorShape>> {
        lock(&self.shapes)
            .iter()
            .find(|shape| shape.shape_id == shape_id)
            .cloned()
    }
}

impl CursorSink for CursorSlot {
    fn cursor_moved(&self, cursor: CursorState, shape: Option<Arc<CursorShape>>) {
        self.update(cursor, shape);
    }
}

/// Presentación en la `Surface`: suelta cada fotograma de MediaCodec a la pantalla.
struct SurfaceSink {
    cursor: Arc<CursorSlot>,
    shown: bool,
}

impl FrameSink for SurfaceSink {
    fn present(&mut self, frame: DecodedFrame) -> Result<(), SinkError> {
        if let Some(cursor) = frame.cursor {
            self.cursor.update(cursor, frame.cursor_shape.clone());
        }
        self.shown = match &frame.frame.gpu {
            Some(GpuImage::MediaCodec(image)) => image.present().shown(),
            // Solo hay decodificadores de MediaCodec: otra imagen no se puede mostrar.
            _ => false,
        };
        Ok(())
    }

    fn shown(&self) -> bool {
        self.shown
    }

    fn cursor_sink(&self) -> Option<Arc<dyn CursorSink>> {
        Some(Arc::clone(&self.cursor) as Arc<dyn CursorSink>)
    }
}

/// La vibración de los mandos va a la aplicación como evento.
struct EventMotors(Emit);

impl AndroidMotors for EventMotors {
    fn set(&mut self, device_id: i32, slot: u8, rumble: Rumble) -> Result<(), String> {
        (self.0)(UiEvent::Rumble {
            device_id,
            slot,
            strong: rumble.strong,
            weak: rumble.weak,
        });
        Ok(())
    }
}

/// Lo que dura más que una sesión: los mandos conectados, el umbral de los gestos y el modo del
/// tacto, la superficie y los decodificadores del teléfono.
pub struct Devices {
    /// Mandos conectados: id de `InputDevice` y su clave.
    pub pads: Mutex<Vec<(i32, PadKey)>>,
    /// Umbrales de `ViewConfiguration`.
    pub touch_config: Mutex<TouchConfig>,
    /// Modo del tacto.
    pub touch_mode: Mutex<TouchMode>,
    /// Tamaño de la vista del vídeo.
    pub view: Mutex<(u32, u32)>,
    /// La superficie de la vista del vídeo.
    pub surface: SurfaceSlot,
    /// Decodificadores de `MediaCodecList`.
    pub decoders: Mutex<Vec<AndroidDecoderInfo>>,
}

impl Default for Devices {
    fn default() -> Self {
        Self {
            pads: Mutex::new(Vec::new()),
            // Los valores de AOSP hasta que la aplicación dé los del dispositivo.
            touch_config: Mutex::new(TouchConfig::new(24.0, 100, 400, 300)),
            touch_mode: Mutex::new(TouchMode::Trackpad),
            view: Mutex::new((0, 0)),
            surface: SurfaceSlot::new(),
            decoders: Mutex::new(Vec::new()),
        }
    }
}

/// Una sesión en una sala.
pub struct Guest {
    session: Mutex<Option<RemoteSession>>,
    input: GuestInput,
    audio: AudioControl,
    touch: Mutex<TouchMapper>,
    pads: Arc<Mutex<AndroidPads>>,
    cursor: Arc<CursorSlot>,
    relayed: AtomicBool,
    stop: Arc<AtomicBool>,
    poller: Mutex<Option<thread::JoinHandle<()>>>,
}

/// Configuración de la sesión con la cuenta guardada.
fn remote_config(
    store: &dyn AccountStorage,
    server: Option<String>,
) -> Result<RemoteConfig, String> {
    let options = ServerOptions {
        server,
        trust_fingerprint: None,
        tcp_port: None,
    };
    let session = account::session(store, &options)?;
    Ok(RemoteConfig::new(
        session.credentials.server.clone(),
        session.credentials.token.clone(),
        session.trust,
    ))
}

/// Audio de la sala: el juego por AAudio y, si se pidió, el micrófono con el preset de voz.
fn guest_audio(request: &JoinRequest, emit: &Emit) -> GuestAudio {
    let speaker = match coroom_session::audio::playback_sink(None, AudioFormat::GAME) {
        Ok(sink) => Some(sink),
        Err(err) => {
            emit(UiEvent::Warning {
                message: format!("sin audio del juego: {err}"),
            });
            None
        }
    };
    let voice = if request.mic {
        match coroom_session::audio::mic_source(None) {
            Ok(mic) => Some(VoiceInput {
                talk: TalkMode::VoiceActivity,
                ..VoiceInput::new(mic)
            }),
            Err(err) => {
                emit(UiEvent::Warning {
                    message: format!("sin micrófono: {err}"),
                });
                None
            }
        }
    } else {
        None
    };
    GuestAudio {
        speaker,
        voice,
        mute_game: false,
        frame_ms: 10,
    }
}

impl Guest {
    /// Entra en la sala de `request` y reenvía sus eventos a `emit` hasta que termine.
    pub fn join(
        store: &dyn AccountStorage,
        devices: &Arc<Devices>,
        request: &JoinRequest,
        emit: Emit,
    ) -> Result<Arc<Guest>, String> {
        let config = remote_config(store, request.server.clone())?;
        let (identity, _) = store.load_identity()?;
        let decoders =
            DecoderSet::android(lock(&devices.decoders).clone(), devices.surface.clone());
        if decoders.is_empty() {
            return Err(
                "este teléfono no decodifica por hardware ningún códec que use coroom".to_owned(),
            );
        }
        let cursor = Arc::new(CursorSlot::default());
        let sink = SurfaceSink {
            cursor: Arc::clone(&cursor),
            shown: false,
        };
        let mut session = RemoteSession::guest(
            identity,
            config,
            request.code.trim(),
            decoders,
            Box::new(sink),
            None,
            Arc::new(LatencyLedger::new()),
        )
        .with_guest_audio(guest_audio(request, &emit));
        session
            .start()
            .map_err(|err| format!("no se pudo entrar en la sala: {err}"))?;
        emit(UiEvent::JoinProgress {
            step: "server",
            done: true,
        });

        let input = session.input();
        let audio = session.audio();
        audio.set_mic_muted(request.mic_muted);
        audio.set_game_gain(request.game_volume.min(200) as f32 / 100.0);
        let mut touch = TouchMapper::new(*lock(&devices.touch_config));
        let _ = touch.set_mode(*lock(&devices.touch_mode));
        let (width, height) = *lock(&devices.view);
        touch.set_view(width, height);
        let link: Arc<dyn PadLink> = Arc::new(input.clone());
        let mut pads = AndroidPads::new(
            link,
            PadChoice::Auto,
            Box::new(EventMotors(Arc::clone(&emit))),
        );
        for (device_id, key) in lock(&devices.pads).iter() {
            pads.device_added(*device_id, key.clone());
        }
        let events = session.events();
        let guest = Arc::new(Guest {
            session: Mutex::new(Some(session)),
            input,
            audio,
            touch: Mutex::new(touch),
            pads: Arc::new(Mutex::new(pads)),
            cursor,
            relayed: AtomicBool::new(false),
            stop: Arc::new(AtomicBool::new(false)),
            poller: Mutex::new(None),
        });
        guest.spawn_poller(Arc::clone(&emit));
        let forwarding = Arc::clone(&guest);
        thread::Builder::new()
            .name("coroom-eventos".to_owned())
            .spawn(move || forwarding.forward(&events, &emit))
            .map_err(|err| format!("no arrancó la sesión: {err}"))?;
        Ok(guest)
    }

    /// Reenvía los eventos de la sesión hasta que termine.
    fn forward(&self, events: &crossbeam_channel::Receiver<SessionEvent>, emit: &Emit) {
        let mut ended = false;
        while let Ok(event) = events.recv() {
            match &event {
                SessionEvent::Warning(_) | SessionEvent::RelayUnavailable(_) => {
                    tracing::warn!(%event, "aviso de la sesión");
                }
                _ => tracing::info!(%event, "sesión"),
            }
            match &event {
                SessionEvent::Connected { path, .. } | SessionEvent::Reconnected { path } => {
                    self.relayed
                        .store(*path == Path::Relayed, Ordering::Release);
                    emit(UiEvent::JoinProgress {
                        step: "path",
                        done: true,
                    });
                }
                SessionEvent::LinkProbe { .. } => emit(UiEvent::JoinProgress {
                    step: "probe",
                    done: true,
                }),
                SessionEvent::Joined { .. } => emit(UiEvent::JoinProgress {
                    step: "keys",
                    done: true,
                }),
                _ => {}
            }
            ended |= matches!(event, SessionEvent::Ended(_));
            if let Some(event) = events::from_session(&event) {
                emit(event);
            }
        }
        if !ended {
            let error = lock(&self.session)
                .as_ref()
                .and_then(RemoteSession::end_reason)
                .map(|err| err.to_string());
            emit(UiEvent::Ended { error });
        }
        self.stop.store(true, Ordering::Release);
    }

    fn spawn_poller(self: &Arc<Self>, emit: Emit) {
        let guest = Arc::downgrade(self);
        let stop = Arc::clone(&self.stop);
        let spawned = thread::Builder::new()
            .name("coroom-mandos".to_owned())
            .spawn(move || {
                let mut allowed: Option<Allowed> = None;
                while !stop.load(Ordering::Acquire) {
                    let Some(guest) = guest.upgrade() else {
                        return;
                    };
                    let now = guest.input.allowed();
                    if allowed != Some(now) {
                        lock(&guest.pads).set_slot(now.gamepad_slot);
                        if allowed.is_some_and(|before| before.keyboard_mouse)
                            && !now.keyboard_mouse
                        {
                            for event in lock(&guest.touch).reset() {
                                guest.input.send(event);
                            }
                        }
                        emit(UiEvent::Permissions {
                            keyboard_mouse: now.keyboard_mouse,
                            gamepad_slot: now.gamepad_slot,
                        });
                        allowed = Some(now);
                    }
                    if let Some((width, height)) = guest.input.host_frame_size() {
                        lock(&guest.touch).set_host_frame(width, height);
                    }
                    lock(&guest.pads).poll();
                    drop(guest);
                    thread::sleep(IDLE_POLL);
                }
            });
        match spawned {
            Ok(handle) => *lock(&self.poller) = Some(handle),
            Err(err) => tracing::warn!(error = %err, "sin hilo de mandos"),
        }
    }

    /// Sale de la sala y espera a que se suelte todo.
    pub fn leave(&self) {
        self.stop.store(true, Ordering::Release);
        for event in lock(&self.touch).reset() {
            self.input.send(event);
        }
        if let Some(mut session) = lock(&self.session).take() {
            session.close();
        }
        if let Some(poller) = lock(&self.poller).take() {
            let _ = poller.join();
        }
    }

    /// Un `MotionEvent` del tacto sobre el vídeo. Devuelve cuándo llamar a [`Guest::touch_tick`]
    /// (reloj de `getEventTime()`), o `None`.
    pub fn touch(
        &self,
        action_masked: i32,
        action_pointer_id: i32,
        pointers: &[TouchPointer],
        time_ms: u64,
    ) -> Option<u64> {
        let action = TouchAction::from_android(action_masked, action_pointer_id)?;
        let mut touch = lock(&self.touch);
        for event in touch.on_touch(action, pointers, time_ms) {
            self.input.send(event);
        }
        touch.next_deadline()
    }

    /// Decide el gesto en curso a `time_ms` (dedo quieto).
    pub fn touch_tick(&self, time_ms: u64) -> Option<u64> {
        let mut touch = lock(&self.touch);
        for event in touch.tick(time_ms) {
            self.input.send(event);
        }
        touch.next_deadline()
    }

    /// Cambia el modo del tacto.
    pub fn set_touch_mode(&self, mode: TouchMode) {
        for event in lock(&self.touch).set_mode(mode) {
            self.input.send(event);
        }
    }

    /// Umbrales nuevos de los gestos.
    pub fn set_touch_config(&self, config: TouchConfig) {
        lock(&self.touch).set_config(config);
    }

    /// Tamaño de la vista del vídeo.
    pub fn set_view(&self, width: u32, height: u32) {
        lock(&self.touch).set_view(width, height);
    }

    /// Una tecla del teclado (no de un mando). `false` si no tiene equivalente.
    pub fn key(&self, keycode: i32, scancode: i32, pressed: bool) -> bool {
        let Some(code) = evdev_from_android(keycode, scancode) else {
            return false;
        };
        self.input.send(InputEvent::Key { code, pressed });
        true
    }

    /// Un botón de un mando.
    pub fn pad_key(&self, device_id: i32, keycode: i32, pressed: bool, at: Instant) -> bool {
        lock(&self.pads).on_key(device_id, keycode, pressed, at)
    }

    /// Los ejes de un mando.
    pub fn pad_motion(&self, device_id: i32, axes: &[f32], at: Instant) -> bool {
        lock(&self.pads).on_motion(device_id, &PadAxes::from_values(axes), at)
    }

    /// Se conectó un mando.
    pub fn pad_added(&self, device_id: i32, key: PadKey) {
        lock(&self.pads).device_added(device_id, key);
    }

    /// Se desconectó un mando.
    pub fn pad_removed(&self, device_id: i32) {
        lock(&self.pads).device_removed(device_id);
    }

    /// El control del audio.
    pub fn audio(&self) -> &AudioControl {
        &self.audio
    }

    /// Envía un mensaje al chat.
    pub fn send_chat(&self, text: &str) -> Result<(), String> {
        lock(&self.session)
            .as_ref()
            .ok_or_else(|| "ya no estás en la sala".to_owned())?
            .send_chat(text)
            .map_err(|err| err.to_string())
    }

    /// Cifras del enlace para la pastilla de conexión.
    pub fn stats(&self) -> serde_json::Value {
        let session = lock(&self.session);
        let Some(session) = session.as_ref() else {
            return serde_json::Value::Null;
        };
        let summary = session.summary();
        let link = session.link();
        serde_json::json!({
            "rtt_ms": link.as_ref().map(|link| link.rtt_ms),
            "loss_percent": link.as_ref().map(|link| link.loss_percent),
            "target_kbps": link.as_ref().map(|link| link.target_kbps),
            "relayed": self.relayed.load(Ordering::Acquire),
            "fps": summary.fps,
            "kbps": summary.bitrate_kbps,
            "latency_ms": summary.total.p50_us as f64 / 1000.0,
            "latency_p95_ms": summary.total.p95_us as f64 / 1000.0,
        })
    }

    /// El cursor del anfitrión en píxeles de la vista: número de cambio, posición, si se ve y su
    /// forma. `None` sin cursor o sin saber dónde está el vídeo.
    pub fn cursor(&self) -> Option<(u64, f64, f64, bool, u64)> {
        let (seq, state) = self.cursor.current();
        let state = state?;
        let rect = lock(&self.touch).video_rect()?;
        let (frame_w, _) = self.input.host_frame_size()?;
        let scale = rect.width / f64::from(frame_w.max(1));
        Some((
            seq,
            rect.x + f64::from(state.x) * scale,
            rect.y + f64::from(state.y) * scale,
            state.visible,
            state.shape_id,
        ))
    }

    /// La forma de cursor `shape_id`, escalada como el vídeo: ancho, alto, punto activo y los
    /// píxeles en ARGB (lo que pide `Bitmap.createBitmap(int[], …)`).
    pub fn cursor_shape(&self, shape_id: u64) -> Option<(u32, u32, f64, u32, u32, Vec<i32>)> {
        let shape = self.cursor.shape(shape_id)?;
        let scale = match (lock(&self.touch).video_rect(), self.input.host_frame_size()) {
            (Some(rect), Some((frame_w, _))) => rect.width / f64::from(frame_w.max(1)),
            _ => 1.0,
        };
        let pixels = shape
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, a]| i32::from_be_bytes([a, r, g, b]))
            .collect();
        Some((
            shape.width,
            shape.height,
            scale,
            shape.hotspot_x,
            shape.hotspot_y,
            pixels,
        ))
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Mandos conectados para Ajustes.
pub fn pad_list(devices: &Devices) -> serde_json::Value {
    let pads = lock(&devices.pads);
    let mut seen: HashMap<&PadKey, ()> = HashMap::new();
    let list: Vec<_> = pads
        .iter()
        .filter(|(_, key)| seen.insert(key, ()).is_none())
        .map(|(device_id, key)| {
            serde_json::json!({
                "device_id": device_id,
                "name": key.name,
                "vendor": key.vendor,
                "product": key.product,
            })
        })
        .collect();
    serde_json::Value::from(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(x: i32, shape_id: u64) -> CursorState {
        CursorState {
            x,
            y: 10,
            visible: true,
            shape_id,
        }
    }

    fn shape(shape_id: u64) -> Arc<CursorShape> {
        Arc::new(CursorShape {
            shape_id,
            width: 1,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: vec![0x11, 0x22, 0x33, 0x44],
        })
    }

    #[test]
    fn the_cursor_slot_counts_changes_and_keeps_recent_shapes() {
        let slot = CursorSlot::default();
        assert_eq!(slot.current(), (0, None));
        slot.update(state(1, 1), Some(shape(1)));
        slot.update(state(1, 1), None);
        assert_eq!(slot.current().0, 1, "lo mismo no cuenta como cambio");
        slot.update(state(2, 1), None);
        assert_eq!(slot.current(), (2, Some(state(2, 1))));
        for id in 2..=(CURSOR_SHAPES as u64 + 1) {
            slot.update(state(3, id), Some(shape(id)));
        }
        assert!(slot.shape(1).is_none(), "la más vieja se olvida");
        assert!(slot.shape(CURSOR_SHAPES as u64 + 1).is_some());
    }

    #[test]
    fn pads_are_listed_once_per_key() {
        let devices = Devices::default();
        let key = PadKey {
            vendor: 0x045e,
            product: 0x0b13,
            name: "Mando de ejemplo".to_owned(),
        };
        lock(&devices.pads).push((5, key.clone()));
        lock(&devices.pads).push((6, key));
        let list = pad_list(&devices);
        assert_eq!(list.as_array().map(Vec::len), Some(1));
        assert_eq!(list[0]["name"], "Mando de ejemplo");
    }
}
