//! Audio del juego y voz de la sala en la sesión remota.
//!
//! **Emisión** (hilos `audio-cap` y `voice-cap`): fuente → (procesado y VAD o pulsar para hablar,
//! en la voz) → Opus → [`AudioHeader`] + paquete → **SFrame** con la clave del emisor y la pista
//! como metadato → un datagrama por destino (`coroom_transport::media::audio_datagram`). La marca
//! `capture_us` va en el **reloj del anfitrión**: el del propio anfitrión (el origen común de todos
//! sus `MediaSender`) o, en el invitado, su hora convertida con el desfase de `Ping`/`Pong`.
//!
//! **Recepción** (hilo `audio-net`, propio y en tiempo real, fuera del runtime de la red):
//! `read_audio_datagram_blocking` → SFrame (que dice el emisor y la pista) →
//! cabecera de audio → canal hacia el hilo `audio-play`. En el anfitrión, la voz de un invitado con
//! permiso se **reenvía tal cual** (el mismo datagrama cifrado, sin descifrarlo de nuevo ni
//! recifrarlo) a los demás invitados con permiso; el anfitrión la descifra solo para oírla y para
//! comprobar quién la envía.
//!
//! **Reproducción** (hilo `audio-play`): un [`JitterBuffer`] y un [`OpusDecoder`] por emisor y pista
//! (PLC, FEC en banda o DRED en las pérdidas), mezcla ([`Mixer`], con el juego silenciable) y el
//! sumidero, que marca el ritmo. En el invitado, [`AvSync`] compara cada trama del juego con el
//! fotograma de captura más cercana y ajusta el buffer del juego cada [`RECOMMEND_PERIOD`]
//! (100 ms), sin retrasar el audio más de su tope ([`coroom_audio::sync::DEFAULT_MAX_DELAY`]).
//! Cada trama deja sus marcas en el [`LatencyLedger`] ([`AudioStage`]) y los contadores se
//! publican cada medio segundo.
//!
//! Sin asignaciones en caliente: los búferes PCM, Opus, SFrame y de paquetes se reservan al
//! arrancar (los paquetes viajan por el canal por valor, con su carga en un array). La única
//! asignación por trama es el `Bytes` del datagrama que se entrega a `quinn`, que se queda con él
//! hasta enviarlo (y que se comparte entre todos los destinos). La reproducción, que a demanda
//! corre en el hilo de tiempo real del dispositivo, tampoco asigna ni toma cerrojos: ni los del
//! registro (las marcas, los contadores, los percentiles de la sincronía, los emisores nuevos
//! (preparados de antemano) y los olvidados van y vienen por canales acotados a un hilo de
//! contadores: el propio `audio-play` a demanda, `audio-stats` con la cola del sumidero), ni el de
//! las voces silenciadas ni el de sus volúmenes (huecos atómicos, [`ControlInner::muted_voices`]
//! y [`ControlInner::voice_gains`]; el del juego, un atómico, [`ControlInner::game_gain`]), ni el
//! del reloj de la sesión más que una vez cada
//! [`SYNC_TICKS`] tramas para renovar el ancla con la que convierte las marcas de captura
//! ([`Playout::local_capture`]). Lo único que ordena es la copia de 200 tránsitos del buffer de
//! jitter, en su propia memoria.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use coroom_audio::codec::{OpusDecoder, OpusEncoder, Profile, Recovery, is_silk_or_hybrid};
use coroom_audio::jitter::{JitterBuffer, JitterStats, Pop};
use coroom_audio::mix::Mixer;
use coroom_audio::processing::{ProcessingOptions, RenderFeed, VoiceProcessor};
use coroom_audio::sync::{AvSync, RECOMMEND_PERIOD};
use coroom_audio::{
    AudioConfig, AudioError, AudioFormat, AudioSink, AudioSource, DeviceCounters,
    MAX_FRAME_SAMPLES, Render,
};
use coroom_transport::PeerConnection;
use coroom_transport::crypto::sframe::FrameEncryptor;
use coroom_transport::proto::audio::{AUDIO_HEADER_LEN, AudioHeader};
use coroom_transport::proto::media::Track;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use tracing::debug;

use super::{Core, SessionEvent, lock};
use crate::latency::{
    AudioCounters, AudioSide, AudioStage, AudioTimeline, AudioTrack, AvSummary, LatencyLedger,
};

/// Bytes máximos de un paquete Opus en el cable.
pub const MAX_PACKET: usize = coroom_audio::jitter::MAX_PACKET;

/// Paquetes en vuelo entre la red y la reproducción.
const PACKET_QUEUE: usize = 512;

/// Cada cuántas tramas de reproducción se publican los contadores (medio segundo).
const PUBLISH_TICKS: u32 = 50;

/// Cada cuántas tramas de reproducción se reajusta la sincronía A/V ([`RECOMMEND_PERIOD`], 100 ms).
const SYNC_TICKS: u32 =
    (RECOMMEND_PERIOD.as_millis() / coroom_audio::FRAME_DURATION.as_millis()) as u32;

/// Un emisor que no envía en este tiempo se olvida (su buffer y su decodificador).
const IDLE_SOURCE: Duration = Duration::from_secs(60);

/// Cada cuánto mira `audio-play` si el dispositivo a demanda sigue en marcha.
const PULL_WATCH: Duration = Duration::from_millis(20);

/// Emisores de voz como mucho a la vez.
const MAX_VOICES: usize = 16;

/// Voces que se pueden tener silenciadas a la vez (huecos de [`ControlInner::muted_voices`]).
const MUTED_SLOTS: usize = 2 * MAX_VOICES;

/// Marca de un hueco de voz silenciada ocupado (el emisor va en los 32 bits bajos).
const MUTED_USED: u64 = 1 << 32;

/// Voces con un volumen distinto del normal a la vez (huecos de [`ControlInner::voice_gains`]).
const GAIN_SLOTS: usize = 2 * MAX_VOICES;

/// Marca de un hueco de volumen ocupado: es el bit de signo de la ganancia (que nunca es negativa)
/// en los 32 bits altos, donde van los bits del `f32`; el emisor va en los 32 bits bajos.
const GAIN_USED: u64 = 1 << 63;

/// Ganancia máxima de una voz, y del juego (lineal: 2 es el doble de volumen; el limitador del
/// mezclador evita que recorte).
pub const MAX_VOICE_GAIN: f32 = 2.0;

/// Fuente del juego en el mezclador.
const GAME_SOURCE: u32 = u32::MAX;

/// Bitrate por defecto del juego (bit/s).
pub const DEFAULT_GAME_BITRATE: u32 = 128_000;

/// Bitrate por defecto de la voz (bit/s).
pub const DEFAULT_VOICE_BITRATE: u32 = 32_000;

/// Sin dispositivo que funcione, cada cuánto se mira si ya se eligió otro.
const DEAD_DEVICE_POLL: Duration = Duration::from_millis(20);

/// Cada cuánto mira quien pide un cambio de dispositivo si el hilo de audio sigue ahí.
const SWITCH_POLL: Duration = Duration::from_millis(50);

/// Lo que se espera a que el hilo de audio recoja el dispositivo nuevo (lo mira en cada trama).
const SWITCH_PICKUP: Duration = Duration::from_secs(2);

/// Lo que se espera, como mucho, a que el hilo de audio diga si la salida nueva abrió (abrirla
/// tarda hasta 5 s con WASAPI y, si falla, volver a abrir la anterior otro tanto).
const SWITCH_ANSWER: Duration = Duration::from_secs(15);

/// Por qué no se pudo cambiar de dispositivo con la sala abierta ([`AudioControl::replace_mic`],
/// [`AudioControl::replace_speaker`]). La interfaz lo explica con sus propios textos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceSwitchError {
    /// La sesión no tiene ese dispositivo en marcha: se entró sin micrófono (o sin salida) o la
    /// sesión ya terminó. Lo elegido vale para la próxima.
    NotRunning,
    /// El dispositivo nuevo no se pudo abrir; sigue el que había.
    Open(AudioError),
    /// La salida nueva funciona de otra manera que la que había (a demanda o con cola).
    Incompatible,
    /// El hilo de audio no contestó a tiempo.
    Timeout,
}

impl std::fmt::Display for DeviceSwitchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => f.write_str("la sala no usa ese dispositivo"),
            Self::Open(err) => write!(f, "no se pudo abrir: {err}"),
            Self::Incompatible => f.write_str("la salida nueva funciona de otra manera"),
            Self::Timeout => f.write_str("el audio de la sala no contestó a tiempo"),
        }
    }
}

impl std::error::Error for DeviceSwitchError {}

/// Un dispositivo de la sala que no funciona: la sesión sigue (sin voz o sin oír nada) hasta que
/// se elija otro ([`SessionEvent::Audio`]). Se avisa una vez por fallo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioIssue {
    /// El micrófono no se pudo abrir al entrar.
    MicUnavailable(AudioError),
    /// El micrófono dejó de funcionar (se desconectó, el sistema lo invalidó…).
    MicLost(AudioError),
    /// La salida no se pudo abrir al entrar.
    SpeakerUnavailable(AudioError),
    /// La salida dejó de funcionar.
    SpeakerLost(AudioError),
}

impl std::fmt::Display for AudioIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MicUnavailable(err) => write!(f, "sin micrófono: {err}"),
            Self::MicLost(err) => write!(f, "el micrófono se detuvo: {err}"),
            Self::SpeakerUnavailable(err) => write!(f, "sin salida de audio: {err}"),
            Self::SpeakerLost(err) => write!(f, "la salida de audio se detuvo: {err}"),
        }
    }
}

/// Avisa de un dispositivo que no funciona.
fn report(core: &Core, issue: AudioIssue) {
    core.emit(SessionEvent::Audio(issue));
}

/// Cómo se decide cuándo se habla.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TalkMode {
    /// Activación por voz (VAD con histéresis), la opción por defecto.
    #[default]
    VoiceActivity,
    /// Pulsar para hablar (`--ptt TECLA`): solo mientras la tecla está pulsada.
    PushToTalk,
}

/// Micrófono y su procesado.
pub struct VoiceInput {
    /// Micrófono.
    pub mic: Box<dyn AudioSource>,
    /// Procesado (NS, AGC, AEC).
    pub processing: ProcessingOptions,
    /// Activación por voz o pulsar para hablar.
    pub talk: TalkMode,
    /// Bitrate de la voz en bit/s (32-48 kbit/s).
    pub bitrate: u32,
}

impl VoiceInput {
    /// Micrófono con el procesado completo, activación por voz y 32 kbit/s.
    pub fn new(mic: Box<dyn AudioSource>) -> Self {
        Self {
            mic,
            processing: ProcessingOptions::default(),
            talk: TalkMode::VoiceActivity,
            bitrate: DEFAULT_VOICE_BITRATE,
        }
    }
}

/// Audio del anfitrión ([`super::RemoteSession::with_host_audio`]).
#[derive(Default)]
pub struct HostAudio {
    /// Fuente del audio del juego (`None`: sin audio del juego).
    pub game: Option<Box<dyn AudioSource>>,
    /// Bitrate del juego en bit/s (128-160 kbit/s).
    pub game_bitrate: u32,
    /// Duración de cada paquete: 10 ms (por defecto) o 20 ms si la tasa de paquetes aprieta.
    pub frame_ms: u8,
    /// Voz propia (`None`: el anfitrión no habla).
    pub voice: Option<VoiceInput>,
    /// Dónde suena la voz de los invitados (`None`: no se oye; se sigue reenviando).
    pub speaker: Option<Box<dyn AudioSink>>,
}

/// Audio del invitado ([`super::RemoteSession::with_guest_audio`]).
#[derive(Default)]
pub struct GuestAudio {
    /// Salida del juego y de las voces (estéreo; `None`: no se oye nada).
    pub speaker: Option<Box<dyn AudioSink>>,
    /// Voz propia (`--voice`; se envía solo con permiso del anfitrión).
    pub voice: Option<VoiceInput>,
    /// Empieza con el juego silenciado (`--mute-game`).
    pub mute_game: bool,
    /// Duración de cada paquete de voz (10 o 20 ms).
    pub frame_ms: u8,
}

#[derive(Debug, Default)]
struct ControlInner {
    mic_muted: AtomicBool,
    game_muted: AtomicBool,
    push_to_talk: AtomicBool,
    talk_pressed: AtomicBool,
    voice_allowed: AtomicBool,
    voice_enabled: AtomicBool,
    speaking: AtomicBool,
    talkers: Mutex<BTreeSet<u32>>,
    /// Voces que este equipo no quiere oír, por emisor (la hoja SFrame en el invitado, el id del
    /// invitado en el anfitrión). Solo afecta a lo que se reproduce aquí. Huecos atómicos
    /// ([`MUTED_USED`] | emisor, o 0 si está libre) en vez de un conjunto con cerrojo: la
    /// reproducción los consulta por cada voz y trama desde el hilo del dispositivo.
    muted_voices: [AtomicU64; MUTED_SLOTS],
    /// Volumen de cada voz en este equipo, por emisor, cuando no es el normal (1.0): huecos
    /// atómicos ([`GAIN_USED`] | bits de la ganancia << 32 | emisor, o 0 si está libre), por lo
    /// mismo que [`ControlInner::muted_voices`]. Una voz sin hueco suena a 1.0.
    voice_gains: [AtomicU64; GAIN_SLOTS],
    /// Volumen del audio del juego en este equipo, aparte de su silencio
    /// ([`ControlInner::game_muted`], que manda).
    game_gain: GameGain,
    /// Volumen de la voz propia que se envía (lo alto que la oyen los demás), aparte del
    /// silencio del micrófono.
    mic_gain: GameGain,
    /// Micrófono nuevo elegido en marcha, **ya abierto**, que adopta el hilo `voice-cap`: le
    /// devuelve el anterior por el canal, para soltarlo fuera del tiempo real.
    new_mic: Pending<MicSwap>,
    /// Salida nueva elegida en marcha, que adopta la reproducción: contesta por el canal si abrió.
    new_speaker: Pending<SpeakerSwap>,
    /// Formato de la salida en uso (para abrir otra igual) mientras corre la reproducción.
    speaker_format: Mutex<Option<AudioFormat>>,
    /// Corre el hilo `voice-cap` (aunque su micrófono no funcione).
    mic_running: AtomicBool,
    /// Un cambio de dispositivo a la vez.
    switching: Mutex<()>,
}

/// Un micrófono ya abierto camino de `voice-cap` y por dónde devolver el anterior.
type MicSwap = (Box<dyn AudioSource>, Sender<Box<dyn AudioSource>>);

/// Una salida camino de la reproducción y por dónde contestar si abrió.
type SpeakerSwap = (Box<dyn AudioSink>, Sender<Result<(), DeviceSwitchError>>);

/// Marca que un hilo de audio corre mientras vive (y que no, al terminar por donde sea).
struct RunningFlag<'a>(&'a AtomicBool);

impl<'a> RunningFlag<'a> {
    fn new(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::Release);
        Self(flag)
    }
}

impl Drop for RunningFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Un dispositivo pedido en marcha que adopta el hilo que lo usa: el aviso es un atómico, para
/// que ese hilo lo mire en cada trama sin tomar el cerrojo.
struct Pending<T> {
    ready: AtomicBool,
    value: Mutex<Option<T>>,
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Self {
            ready: AtomicBool::new(false),
            value: Mutex::new(None),
        }
    }
}

impl<T> std::fmt::Debug for Pending<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("ready", &self.ready.load(Ordering::Relaxed))
            .finish()
    }
}

impl<T> Pending<T> {
    fn put(&self, value: T) {
        *lock(&self.value) = Some(value);
        self.ready.store(true, Ordering::Release);
    }

    fn take(&self) -> Option<T> {
        if !self.ready.swap(false, Ordering::AcqRel) {
            return None;
        }
        lock(&self.value).take()
    }
}

/// Ganancia del juego (o del micrófono): los bits del `f32` en un atómico, para que la
/// reproducción o la captura la lean sin cerrojo por cada trama; 1.0 (lo normal) de partida.
#[derive(Debug)]
struct GameGain(AtomicU32);

impl Default for GameGain {
    fn default() -> Self {
        Self(AtomicU32::new(1.0f32.to_bits()))
    }
}

/// Hueco de [`ControlInner::voice_gains`] del emisor `source` con la ganancia `gain` (acotada).
fn gain_key(source: u32, gain: f32) -> u64 {
    GAIN_USED | (u64::from(gain.to_bits()) << 32) | u64::from(source)
}

/// Si el hueco `slot` es del emisor `source`.
fn gain_slot_is(slot: u64, source: u32) -> bool {
    slot & GAIN_USED != 0 && slot as u32 == source
}

/// Ganancia guardada en un hueco ocupado.
fn gain_of(slot: u64) -> f32 {
    f32::from_bits(((slot & !GAIN_USED) >> 32) as u32)
}

/// Mandos del audio que la ventana y la consola tocan en caliente (atajos `Ctrl+Alt+M` y
/// `Ctrl+Alt+V`, tecla de pulsar para hablar) y estado que muestra el overlay.
#[derive(Debug, Clone, Default)]
pub struct AudioControl {
    inner: Arc<ControlInner>,
}

impl AudioControl {
    /// Silencia o reactiva el micrófono; devuelve si queda silenciado.
    pub fn toggle_mic(&self) -> bool {
        !self.inner.mic_muted.fetch_xor(true, Ordering::AcqRel)
    }

    /// Silencia (o no) el micrófono.
    pub fn set_mic_muted(&self, muted: bool) {
        self.inner.mic_muted.store(muted, Ordering::Release);
    }

    /// Si el micrófono está silenciado.
    pub fn mic_muted(&self) -> bool {
        self.inner.mic_muted.load(Ordering::Acquire)
    }

    /// Silencia o reactiva el audio del juego; devuelve si queda silenciado.
    pub fn toggle_game(&self) -> bool {
        !self.inner.game_muted.fetch_xor(true, Ordering::AcqRel)
    }

    /// Silencia (o no) el audio del juego.
    pub fn set_game_muted(&self, muted: bool) {
        self.inner.game_muted.store(muted, Ordering::Release);
    }

    /// Si el juego está silenciado.
    pub fn game_muted(&self) -> bool {
        self.inner.game_muted.load(Ordering::Acquire)
    }

    /// Volumen del audio del juego en este equipo: ganancia lineal entre 0.0 y
    /// [`MAX_VOICE_GAIN`] (fuera del rango se acota; no finita cuenta como 1.0). Aparte del
    /// silencio ([`AudioControl::set_game_muted`]), que manda: silenciado no suena sea cual sea
    /// la ganancia, y la conserva para cuando vuelva a oírse.
    pub fn set_game_gain(&self, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, MAX_VOICE_GAIN)
        } else {
            1.0
        };
        self.inner
            .game_gain
            .0
            .store(gain.to_bits(), Ordering::Release);
    }

    /// Ganancia lineal del juego aquí (1.0 si no se tocó). Sin cerrojo: la mezcla la consulta en
    /// el hilo del dispositivo por cada trama.
    pub fn game_gain(&self) -> f32 {
        f32::from_bits(self.inner.game_gain.0.load(Ordering::Relaxed))
    }

    /// Volumen de la voz propia que se envía: ganancia lineal entre 0.0 y [`MAX_VOICE_GAIN`]
    /// (fuera del rango se acota; no finita cuenta como 1.0). Se aplica tras el procesado, así
    /// que el control automático de ganancia no la deshace. Aparte del silencio del micrófono.
    pub fn set_mic_gain(&self, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, MAX_VOICE_GAIN)
        } else {
            1.0
        };
        self.inner
            .mic_gain
            .0
            .store(gain.to_bits(), Ordering::Release);
    }

    /// Ganancia lineal de la voz propia (1.0 si no se tocó).
    pub fn mic_gain(&self) -> f32 {
        f32::from_bits(self.inner.mic_gain.0.load(Ordering::Relaxed))
    }

    /// Cambia de micrófono en marcha, sin reconectar. Bloquea (hasta unos segundos): abre `mic`
    /// en el hilo que llama, nunca en el de la voz, que es de tiempo real y sigue con el anterior
    /// mientras tanto; el hilo de la voz lo adopta antes de su siguiente trama (también si el
    /// anterior dejó de funcionar) y el anterior se suelta aquí. Si `mic` no abre, sigue el
    /// anterior y devuelve el error. Sin voz en la sesión (se entró sin micrófono, o terminó):
    /// [`DeviceSwitchError::NotRunning`].
    pub fn replace_mic(&self, mut mic: Box<dyn AudioSource>) -> Result<(), DeviceSwitchError> {
        let _one = lock(&self.inner.switching);
        let running = || self.inner.mic_running.load(Ordering::Acquire);
        if !running() {
            return Err(DeviceSwitchError::NotRunning);
        }
        mic.start(&AudioConfig::voice())
            .map_err(DeviceSwitchError::Open)?;
        let (back_tx, back_rx) = crossbeam_channel::bounded(1);
        self.inner.new_mic.put((mic, back_tx));
        let deadline = Instant::now() + SWITCH_PICKUP;
        loop {
            match back_rx.recv_timeout(SWITCH_POLL) {
                Ok(mut old) => {
                    old.stop();
                    return Ok(());
                }
                Err(RecvTimeoutError::Disconnected) => return Err(DeviceSwitchError::NotRunning),
                Err(RecvTimeoutError::Timeout) => {
                    let gone = !running();
                    if (gone || Instant::now() >= deadline)
                        && let Some((mut mic, _)) = self.inner.new_mic.take()
                    {
                        // Nadie lo recogió: se suelta aquí.
                        mic.stop();
                        return Err(if gone {
                            DeviceSwitchError::NotRunning
                        } else {
                            DeviceSwitchError::Timeout
                        });
                    }
                    // Si ya lo recogió, el anterior está al llegar.
                }
            }
        }
    }

    /// Cambia de salida en marcha, sin reconectar. Bloquea hasta que la reproducción suelta la
    /// anterior y abre `speaker` (también si la anterior dejó de funcionar), y devuelve si abrió;
    /// si no, vuelve a la anterior. `speaker` tiene que aceptar [`AudioControl::speaker_format`].
    /// Sin reproducción en la sesión: [`DeviceSwitchError::NotRunning`].
    pub fn replace_speaker(&self, speaker: Box<dyn AudioSink>) -> Result<(), DeviceSwitchError> {
        let _one = lock(&self.inner.switching);
        let running = || self.speaker_format().is_some();
        if !running() {
            return Err(DeviceSwitchError::NotRunning);
        }
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        self.inner.new_speaker.put((speaker, reply_tx));
        let started = Instant::now();
        loop {
            match reply_rx.recv_timeout(SWITCH_POLL) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Disconnected) => return Err(DeviceSwitchError::NotRunning),
                Err(RecvTimeoutError::Timeout) => {
                    let gone = !running();
                    if (gone || started.elapsed() >= SWITCH_PICKUP)
                        && self.inner.new_speaker.take().is_some()
                    {
                        // Nadie la recogió (se suelta aquí, sin haberse abierto).
                        return Err(if gone {
                            DeviceSwitchError::NotRunning
                        } else {
                            DeviceSwitchError::Timeout
                        });
                    }
                    if started.elapsed() >= SWITCH_ANSWER {
                        return Err(DeviceSwitchError::Timeout);
                    }
                }
            }
        }
    }

    /// Formato de la salida en uso, para abrir otra igual ([`AudioControl::replace_speaker`]);
    /// `None` si no hay reproducción en esta sesión (aunque su salida no funcione, la hay).
    pub fn speaker_format(&self) -> Option<AudioFormat> {
        *lock(&self.inner.speaker_format)
    }

    /// Tecla de pulsar para hablar pulsada o soltada.
    pub fn set_talk_pressed(&self, pressed: bool) {
        self.inner.talk_pressed.store(pressed, Ordering::Release);
    }

    /// Si se usa pulsar para hablar.
    pub fn push_to_talk(&self) -> bool {
        self.inner.push_to_talk.load(Ordering::Acquire)
    }

    pub(crate) fn set_push_to_talk(&self, enabled: bool) {
        self.inner.push_to_talk.store(enabled, Ordering::Release);
    }

    /// Si este extremo puede hablar (el anfitrión siempre; el invitado, si se lo permiten).
    pub fn voice_allowed(&self) -> bool {
        self.inner.voice_allowed.load(Ordering::Acquire)
    }

    pub(crate) fn set_voice_allowed(&self, allowed: bool) {
        self.inner.voice_allowed.store(allowed, Ordering::Release);
    }

    /// Si este extremo tiene micrófono en la sesión.
    pub fn voice_enabled(&self) -> bool {
        self.inner.voice_enabled.load(Ordering::Acquire)
    }

    pub(crate) fn set_voice_enabled(&self, enabled: bool) {
        self.inner.voice_enabled.store(enabled, Ordering::Release);
    }

    /// Si ahora se envía voz.
    pub fn speaking(&self) -> bool {
        self.inner.speaking.load(Ordering::Acquire)
    }

    /// Cambia el estado de «hablando»; devuelve si cambió.
    pub(crate) fn set_speaking(&self, speaking: bool) -> bool {
        self.inner.speaking.swap(speaking, Ordering::AcqRel) != speaking
    }

    /// Deja de oír (o vuelve a oír) la voz de `source`: la hoja SFrame de un miembro en el
    /// invitado ([`super::RoomMember::leaf`]) o el id de un invitado en el anfitrión. Solo cambia lo
    /// que suena en este equipo; los demás le siguen oyendo.
    pub fn set_voice_muted(&self, source: u32, muted: bool) {
        let key = MUTED_USED | u64::from(source);
        let slots = &self.inner.muted_voices;
        if !muted {
            for slot in slots {
                let _ = slot.compare_exchange(key, 0, Ordering::AcqRel, Ordering::Acquire);
            }
        } else if !slots.iter().any(|slot| slot.load(Ordering::Acquire) == key)
            && !slots.iter().any(|slot| {
                slot.compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
        {
            debug!(source, "sin sitio para silenciar más voces");
        }
    }

    /// Si la voz de `source` está silenciada aquí. Sin cerrojo: se consulta en el hilo del
    /// dispositivo por cada voz y trama.
    pub fn voice_muted(&self, source: u32) -> bool {
        let key = MUTED_USED | u64::from(source);
        self.inner
            .muted_voices
            .iter()
            .any(|slot| slot.load(Ordering::Relaxed) == key)
    }

    /// Volumen de la voz de `source` en este equipo: ganancia lineal entre 0.0 y
    /// [`MAX_VOICE_GAIN`] (1.0, lo normal, libera el hueco; fuera del rango se acota; no finita
    /// cuenta como 1.0). Aparte del silencio ([`AudioControl::set_voice_muted`]): una voz
    /// silenciada conserva su volumen para cuando vuelva a oírse.
    pub fn set_voice_gain(&self, source: u32, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, MAX_VOICE_GAIN)
        } else {
            1.0
        };
        let slots = &self.inner.voice_gains;
        let wanted = (gain != 1.0).then(|| gain_key(source, gain));
        for slot in slots {
            let current = slot.load(Ordering::Acquire);
            if gain_slot_is(current, source) {
                // El mismo hueco cambia de ganancia o se libera.
                let _ = slot.compare_exchange(
                    current,
                    wanted.unwrap_or(0),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                return;
            }
        }
        if let Some(key) = wanted
            && !slots.iter().any(|slot| {
                slot.compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
        {
            debug!(source, "sin sitio para más voces con volumen propio");
        }
    }

    /// Ganancia lineal de la voz de `source` aquí (1.0 si no se tocó). Sin cerrojo: la mezcla la
    /// consulta en el hilo del dispositivo por cada voz y trama.
    pub fn voice_gain(&self, source: u32) -> f32 {
        self.inner
            .voice_gains
            .iter()
            .map(|slot| slot.load(Ordering::Relaxed))
            .find(|slot| gain_slot_is(*slot, source))
            .map_or(1.0, gain_of)
    }

    /// Quién habla en la sala (ids de invitado; 0 = el anfitrión), según `VoiceState`.
    pub fn talkers(&self) -> Vec<u32> {
        lock(&self.inner.talkers).iter().copied().collect()
    }

    pub(crate) fn set_talker(&self, who: u32, speaking: bool) {
        let mut talkers = lock(&self.inner.talkers);
        if speaking {
            talkers.insert(who);
        } else {
            talkers.remove(&who);
        }
    }

    /// Estado local en palabras: «micrófono abierto (activación por voz)», «micrófono
    /// silenciado», «sin permiso de voz», «juego silenciado»…
    pub fn status_label(&self) -> String {
        let mut parts = Vec::new();
        if self.voice_enabled() {
            let mic = if !self.voice_allowed() {
                "sin permiso de voz".to_owned()
            } else if self.mic_muted() {
                "micrófono silenciado".to_owned()
            } else if self.push_to_talk() {
                if self.speaking() {
                    "hablando (pulsar para hablar)".to_owned()
                } else {
                    "pulsar para hablar".to_owned()
                }
            } else if self.speaking() {
                "hablando (activación por voz)".to_owned()
            } else {
                "micrófono abierto (activación por voz)".to_owned()
            };
            parts.push(mic);
        } else {
            parts.push("sin micrófono".to_owned());
        }
        if self.game_muted() {
            parts.push("juego silenciado".to_owned());
        }
        parts.join(" · ")
    }
}

/// Nombre de quien habla: «anfitrión» o «invitado N».
pub fn talker_label(who: u32) -> String {
    if who == 0 {
        "anfitrión".to_owned()
    } else {
        format!("invitado {who}")
    }
}

/// Un paquete de audio recibido, camino de la reproducción (por valor: sin asignar).
#[derive(Clone)]
pub(crate) struct Packet {
    pub(crate) track: AudioTrack,
    /// Emisor: la hoja SFrame en el invitado, el id del invitado en el anfitrión.
    pub(crate) source: u32,
    pub(crate) header: AudioHeader,
    pub(crate) arrived: Instant,
    pub(crate) len: u16,
    pub(crate) data: [u8; MAX_PACKET],
}

impl Packet {
    /// Paquete de `track` del emisor `source` a partir del texto en claro (cabecera + Opus).
    pub(crate) fn parse(
        track: AudioTrack,
        source: u32,
        plain: &[u8],
        arrived: Instant,
    ) -> Option<Packet> {
        let (header, opus) = AudioHeader::parse(plain).ok()?;
        if opus.is_empty() || opus.len() > MAX_PACKET {
            return None;
        }
        let mut data = [0u8; MAX_PACKET];
        data[..opus.len()].copy_from_slice(opus);
        Some(Packet {
            track,
            source,
            header,
            arrived,
            len: opus.len() as u16,
            data,
        })
    }
}

/// Canal hacia `audio-play`.
pub(crate) fn packet_channel() -> (Sender<Packet>, Receiver<Packet>) {
    crossbeam_channel::bounded(PACKET_QUEUE)
}

/// Muestras de 32 bits bajos a µs completos, cerca de `reference` (la hora actual en ese reloj).
pub(crate) fn extend_us(reference: u64, low: u32) -> u64 {
    let delta = (reference as u32).wrapping_sub(low) as i32;
    (reference as i64 - i64::from(delta)).max(0) as u64
}

/// A dónde va lo que se emite.
pub(crate) trait AudioOut: Send + Sync {
    /// Envía el paquete sellado `seq` de `track` a sus destinos. Devuelve si salió hacia alguno.
    fn send(&self, track: Track, seq: u32, sealed: &[u8]) -> bool;
    /// Si hay a quién enviar `track` (si no, no se codifica).
    fn has_targets(&self, track: Track) -> bool;
    /// `at` en µs del reloj del anfitrión, si ya se conoce.
    fn host_us(&self, at: Instant) -> Option<u64>;
    /// Avisa a los demás de si se habla (`VoiceState`).
    fn speaking(&self, speaking: bool);
}

/// Codifica, pone la cabecera y cifra los paquetes de una pista.
struct Sealer {
    encoder: OpusEncoder,
    encryptor: Option<FrameEncryptor>,
    track: Track,
    seq: u32,
    opus: [u8; coroom_audio::codec::MAX_PACKET],
    plain: Vec<u8>,
    sealed: Vec<u8>,
}

impl Sealer {
    fn new(core: &Core, profile: Profile, track: Track, bitrate: u32) -> Result<Self, String> {
        let mut encoder = OpusEncoder::new(profile).map_err(|err| err.to_string())?;
        encoder
            .set_bitrate(bitrate)
            .map_err(|err| err.to_string())?;
        Ok(Self {
            encoder,
            encryptor: core
                .sframe
                .then(|| core.ring.encryptor(core.own_leaf.load(Ordering::Acquire))),
            track,
            seq: 0,
            opus: [0; coroom_audio::codec::MAX_PACKET],
            plain: Vec::with_capacity(AUDIO_HEADER_LEN + coroom_audio::codec::MAX_PACKET),
            sealed: Vec::with_capacity(AUDIO_HEADER_LEN + coroom_audio::codec::MAX_PACKET + 64),
        })
    }

    /// La hoja propia puede cambiar (el invitado la conoce al entrar en el grupo).
    fn refresh_leaf(&mut self, core: &Core) {
        if core.sframe {
            self.encryptor = Some(core.ring.encryptor(core.own_leaf.load(Ordering::Acquire)));
        }
    }

    /// Codifica `pcm` (10 o 20 ms) y lo cifra. Devuelve el paquete sellado.
    fn seal(
        &mut self,
        pcm: &[i16],
        frames: u8,
        capture_us: u32,
        extra_flags: u8,
        timeline: &mut AudioTimeline,
    ) -> Result<&[u8], String> {
        let len = self
            .encoder
            .encode(pcm, &mut self.opus)
            .map_err(|err| err.to_string())?;
        timeline.mark(AudioStage::Encoded, Instant::now());
        let opus = &self.opus[..len];
        let mut flags = extra_flags;
        if self.encoder.fec() && is_silk_or_hybrid(opus) {
            flags |= AudioHeader::FEC;
        }
        if self.encoder.dred() {
            flags |= AudioHeader::DRED;
        }
        let header = AudioHeader {
            seq: self.seq,
            capture_us,
            frames,
            flags,
        };
        self.plain.clear();
        self.plain.extend_from_slice(&header.to_bytes());
        self.plain.extend_from_slice(opus);
        self.seq = self.seq.wrapping_add(1);
        let sealed: &[u8] = match &mut self.encryptor {
            Some(encryptor) => {
                encryptor
                    .encrypt_into(self.track as u8, &self.plain, &mut self.sealed)
                    .map_err(|err| err.to_string())?;
                &self.sealed
            }
            None => &self.plain,
        };
        timeline.mark(AudioStage::Encrypted, Instant::now());
        Ok(sealed)
    }
}

fn frames_of(frame_ms: u8) -> u8 {
    if frame_ms >= 20 { 2 } else { 1 }
}

/// Lee `frames` tramas de 10 ms seguidas de `source` en `pcm`; devuelve la captura de la primera.
fn read_packet(
    source: &mut dyn AudioSource,
    frames: u8,
    frame: &mut Vec<i16>,
    pcm: &mut Vec<i16>,
) -> Result<Instant, coroom_audio::AudioError> {
    pcm.clear();
    let mut first = None;
    for _ in 0..frames {
        let captured = source.read(frame)?;
        first.get_or_insert(captured.captured_at);
        pcm.extend_from_slice(&frame[..captured.samples]);
    }
    Ok(first.unwrap_or_else(Instant::now))
}

/// Hilo `audio-cap` (anfitrión): audio del juego → Opus → SFrame → todos los invitados.
pub(crate) fn game_capture_loop(
    core: &Core,
    mut source: Box<dyn AudioSource>,
    bitrate: u32,
    frame_ms: u8,
    out: &dyn AudioOut,
) {
    realtime("audio-cap");
    let shared = &core.shared;
    if let Err(err) = source.start(&AudioConfig::game()) {
        core.warn(format!("sin audio del juego: {err}"));
        return;
    }
    let mut sealer = match Sealer::new(core, Profile::Game, Track::Audio, bitrate) {
        Ok(sealer) => sealer,
        Err(err) => {
            core.warn(format!("sin audio del juego: {err}"));
            return;
        }
    };
    debug!(fuente = %source.describe(), "audio del juego");
    let frames = frames_of(frame_ms);
    let mut frame = Vec::with_capacity(MAX_FRAME_SAMPLES);
    let mut pcm = Vec::with_capacity(MAX_FRAME_SAMPLES * 2);
    while !shared.stopping() {
        let filter = lock(&core.game_filter).take();
        if let Some(filter) = filter {
            if source.set_app_filter(&filter) {
                debug!(fuente = %source.describe(), "audio del juego cambiado");
            } else {
                core.warn("esta fuente de audio no separa por aplicación: se sigue enviando todo");
            }
        }
        let captured_at = match read_packet(source.as_mut(), frames, &mut frame, &mut pcm) {
            Ok(at) => at,
            Err(err) => {
                core.warn(format!("el audio del juego se detuvo: {err}"));
                break;
            }
        };
        if !out.has_targets(Track::Audio) {
            continue;
        }
        let mut timeline = AudioTimeline::default();
        timeline.mark(AudioStage::Captured, captured_at);
        let capture_us = out.host_us(captured_at).unwrap_or(0) as u32;
        let seq = sealer.seq;
        match sealer.seal(&pcm, frames, capture_us, 0, &mut timeline) {
            Ok(sealed) => {
                if out.send(Track::Audio, seq, sealed) {
                    timeline.mark(AudioStage::Sent, Instant::now());
                    core.ledger
                        .record_audio(AudioTrack::Game, AudioSide::Sender, timeline);
                    core.ledger.record_audio_sent(AudioTrack::Game, 1);
                }
            }
            Err(err) => debug!(error = %err, "trama del juego sin enviar"),
        }
    }
    source.stop();
}

/// Si se envía la trama de voz (y se cuenta como «hablando»): `voice` es lo que dice el VAD. Con
/// el volumen propio a cero no se oiría nada, así que cuenta como silenciado: no se envía ni se
/// dice a los demás que se habla.
fn sends_voice(control: &AudioControl, voice: bool) -> bool {
    let wants = if control.push_to_talk() {
        control.inner.talk_pressed.load(Ordering::Acquire)
    } else {
        voice
    };
    wants && !control.mic_muted() && control.voice_allowed() && control.mic_gain() > 0.0
}

/// Hilo `voice-cap`: micrófono → procesado → VAD o pulsar para hablar → Opus → SFrame → destinos.
/// Si el micrófono no abre o deja de funcionar, avisa una vez ([`AudioIssue`]) y espera a que se
/// elija otro ([`AudioControl::replace_mic`]); solo termina con la sesión.
pub(crate) fn voice_capture_loop(
    core: &Core,
    mut input: VoiceInput,
    mut processor: VoiceProcessor,
    frame_ms: u8,
    control: &AudioControl,
    out: &dyn AudioOut,
) {
    let shared = &core.shared;
    let mut sealer = match Sealer::new(core, Profile::Voice, Track::Voice, input.bitrate) {
        Ok(sealer) => sealer,
        Err(err) => {
            core.warn(format!("sin voz: {err}"));
            control.set_voice_enabled(false);
            return;
        }
    };
    // Desde aquí acepta micrófonos nuevos, aunque el primero no abra o deje de funcionar.
    let _running = RunningFlag::new(&control.inner.mic_running);
    // Se abre antes de pasar a tiempo real: abrir un dispositivo tarda (segundos, si falla).
    let mut live = match input.mic.start(&AudioConfig::voice()) {
        Ok(()) => true,
        Err(err) => {
            report(core, AudioIssue::MicUnavailable(err));
            control.set_voice_enabled(false);
            false
        }
    };
    realtime("voice-cap");
    debug!(micrófono = %input.mic.describe(), procesado = processor.backend(), "voz");
    let frames = frames_of(frame_ms);
    let mut frame = Vec::with_capacity(MAX_FRAME_SAMPLES);
    let mut pcm = Vec::with_capacity(MAX_FRAME_SAMPLES * 2);
    let mut talking = false;
    let mut leaf = core.own_leaf.load(Ordering::Acquire);
    while !shared.stopping() {
        if let Some((mic, back)) = control.inner.new_mic.take() {
            // Llega abierto ([`AudioControl::replace_mic`]); el anterior se suelta allí.
            debug!(micrófono = %mic.describe(), "micrófono cambiado en marcha");
            let old = std::mem::replace(&mut input.mic, mic);
            let _ = back.try_send(old);
            if !live {
                live = true;
                control.set_voice_enabled(true);
            }
        }
        if !live {
            // Sin micrófono que funcione: se espera a que se elija otro.
            std::thread::sleep(DEAD_DEVICE_POLL);
            continue;
        }
        let captured_at = match read_packet(input.mic.as_mut(), frames, &mut frame, &mut pcm) {
            Ok(at) => at,
            Err(err) => {
                // Se avisa una vez y se sigue esperando otro micrófono, sin salir del bucle.
                report(core, AudioIssue::MicLost(err));
                input.mic.stop();
                live = false;
                control.set_voice_enabled(false);
                if talking || control.speaking() {
                    talking = false;
                    if control.set_speaking(false) {
                        out.speaking(false);
                    }
                }
                continue;
            }
        };
        let mut timeline = AudioTimeline::default();
        timeline.mark(AudioStage::Captured, captured_at);
        let mut voice = false;
        for chunk in pcm
            .as_chunks_mut::<{ coroom_audio::SAMPLES_PER_CHANNEL }>()
            .0
        {
            match processor.process(chunk) {
                Ok(result) => voice |= result.speaking,
                Err(err) => debug!(error = %err, "procesado de voz"),
            }
        }
        let gain = control.mic_gain();
        if gain != 1.0 {
            for sample in pcm.iter_mut() {
                *sample = (f32::from(*sample) * gain).clamp(-32768.0, 32767.0) as i16;
            }
        }
        timeline.mark(AudioStage::Processed, Instant::now());
        let now_talking = sends_voice(control, voice);
        if control.set_speaking(now_talking) {
            out.speaking(now_talking);
        }
        // Al callar se envía una última trama con DTX: el receptor sabe que no es una pérdida.
        let send = now_talking || talking;
        let flags = if now_talking { 0 } else { AudioHeader::DTX };
        talking = now_talking;
        if !send || !out.has_targets(Track::Voice) {
            continue;
        }
        let current = core.own_leaf.load(Ordering::Acquire);
        if current != leaf {
            leaf = current;
            sealer.refresh_leaf(core);
        }
        let Some(host_us) = out.host_us(captured_at) else {
            // El invitado aún no conoce el reloj del anfitrión (primer `Pong`).
            continue;
        };
        let seq = sealer.seq;
        match sealer.seal(&pcm, frames, host_us as u32, flags, &mut timeline) {
            Ok(sealed) => {
                if out.send(Track::Voice, seq, sealed) {
                    timeline.mark(AudioStage::Sent, Instant::now());
                    core.ledger
                        .record_audio(AudioTrack::Voice, AudioSide::Sender, timeline);
                    core.ledger.record_audio_sent(AudioTrack::Voice, 1);
                }
            }
            Err(err) => debug!(error = %err, "trama de voz sin enviar"),
        }
    }
    if control.set_speaking(false) {
        out.speaking(false);
    }
    input.mic.stop();
}

/// Lo que suena de un emisor en una pista.
struct Stream {
    track: AudioTrack,
    source: u32,
    channels: u16,
    jitter: JitterBuffer,
    decoder: OpusDecoder,
    /// 20 ms en estéreo como mucho.
    pcm: [i16; MAX_FRAME_SAMPLES * 2],
    /// Muestras de una segunda trama de 10 ms ya decodificada (paquetes de 20 ms).
    pending: usize,
    pending_capture: Option<u32>,
    last_packet: Instant,
}

/// Lo que dio un emisor en esta trama, para marcarlo después de escribirla.
#[derive(Clone, Copy)]
struct Heard {
    track: AudioTrack,
    capture_us: Option<u32>,
    arrived: Option<Instant>,
}

impl Stream {
    fn new(track: AudioTrack, source: u32) -> Result<Self, String> {
        let profile = match track {
            AudioTrack::Game => Profile::Game,
            AudioTrack::Voice => Profile::Voice,
        };
        Ok(Self {
            track,
            source,
            channels: profile.format().channels,
            jitter: JitterBuffer::new(),
            decoder: OpusDecoder::new(profile).map_err(|err| err.to_string())?,
            pcm: [0; MAX_FRAME_SAMPLES * 2],
            pending: 0,
            pending_capture: None,
            last_packet: Instant::now(),
        })
    }

    fn frame_len(&self) -> usize {
        coroom_audio::SAMPLES_PER_CHANNEL * usize::from(self.channels)
    }

    /// Una trama de 10 ms para ahora: sus muestras (en `self.pcm[offset..]`) y lo que se sabe de
    /// ella. `None` si el emisor no tiene nada que sonar.
    fn tick(&mut self, now: Instant) -> Option<(usize, Heard)> {
        let frame = self.frame_len();
        if self.pending > 0 {
            self.pending = 0;
            let capture = self
                .pending_capture
                .take()
                .map(|us| us.wrapping_add(10_000));
            return Some((
                frame,
                Heard {
                    track: self.track,
                    capture_us: capture,
                    arrived: None,
                },
            ));
        }
        let (pcm, decoder) = (&mut self.pcm, &mut self.decoder);
        match self.jitter.pop(now) {
            Pop::Frame(packet) => {
                let samples = coroom_audio::codec::packet_samples(packet.data).unwrap_or(0);
                let frames = (samples / coroom_audio::SAMPLES_PER_CHANNEL).clamp(1, 2);
                let decoded = decoder.decode_frames(Some(packet.data), frames, pcm);
                let heard = Heard {
                    track: self.track,
                    capture_us: Some(packet.capture_us),
                    arrived: Some(packet.arrived),
                };
                match decoded {
                    Ok(n) if n > frame => {
                        self.pending = n - frame;
                        self.pending_capture = Some(packet.capture_us);
                        Some((0, heard))
                    }
                    Ok(_) => Some((0, heard)),
                    Err(_) => {
                        let _ = decoder.decode(None, pcm);
                        Some((
                            0,
                            Heard {
                                capture_us: None,
                                arrived: None,
                                ..heard
                            },
                        ))
                    }
                }
            }
            Pop::Plc(lost) => {
                let lost_frames = lost.frames;
                let (next, fec, dred) = match lost.next {
                    Some((data, flags)) => (
                        Some(data),
                        flags & AudioHeader::FEC != 0,
                        flags & AudioHeader::DRED != 0,
                    ),
                    None => (None, false, false),
                };
                let recovery = decoder
                    .recover(next, fec, dred, pcm)
                    .unwrap_or(Recovery::Plc);
                if !lost.stretch {
                    self.jitter.note_recovery(recovery);
                }
                // Un paquete de 20 ms perdido son dos tramas: la segunda, también ocultada.
                if lost_frames >= 2 && decoder.decode(None, &mut pcm[frame..]).is_ok() {
                    self.pending = frame;
                    self.pending_capture = None;
                }
                Some((
                    0,
                    Heard {
                        track: self.track,
                        capture_us: None,
                        arrived: None,
                    },
                ))
            }
            Pop::Wait => None,
        }
    }
}

/// Lo que necesita `audio-play`.
pub(crate) struct Player {
    pub(crate) sink: Box<dyn AudioSink>,
    pub(crate) control: AudioControl,
    /// AEC: lo que suena va al procesado de la voz propia.
    pub(crate) render: Option<RenderFeed>,
    /// Invitado: sincronía con el vídeo.
    pub(crate) av_sync: bool,
}

/// Faltas y sobras de la salida ([`AudioSink::device_counters`]): las deja el hilo que tiene el
/// sumidero y las publica la reproducción, que a demanda corre en el hilo del dispositivo.
#[derive(Debug, Default)]
struct DeviceStats {
    underruns: AtomicU64,
    overruns: AtomicU64,
}

impl DeviceStats {
    fn store(&self, counters: DeviceCounters) {
        self.underruns.store(counters.underruns, Ordering::Relaxed);
        self.overruns.store(counters.overruns, Ordering::Relaxed);
    }

    fn load(&self) -> DeviceCounters {
        DeviceCounters {
            underruns: self.underruns.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
        }
    }
}

/// Convierte la `capture_us` (32 bits, reloj del anfitrión) de una trama a un instante local.
pub(crate) type ToLocal = dyn Fn(u32) -> Option<Instant> + Send + Sync;

/// Lo que la reproducción deja cada [`PUBLISH_TICKS`] tramas para publicarlo fuera del tiempo
/// real: los contadores de cada buffer y, en el invitado, la sincronía (para sus percentiles).
/// Por valor y de tamaño fijo, como todo lo que cruza los canales de [`Links`].
#[derive(Clone)]
struct Snapshot {
    streams: [(AudioTrack, JitterStats); MAX_VOICES + 1],
    len: usize,
    av: Option<AvSync>,
}

impl Snapshot {
    fn streams(&self) -> impl Iterator<Item = (AudioTrack, JitterStats)> + '_ {
        self.streams[..self.len].iter().copied()
    }
}

/// Canales entre la reproducción (que a demanda corre en el hilo de tiempo real del dispositivo)
/// y el hilo de contadores ([`Housekeeper`]): acotados y reservados al crearlos, y la reproducción
/// solo usa `try_send`/`try_recv`, así no asigna, no espera y no toma los cerrojos del registro.
struct Links {
    /// Marcas de cada trama que sonó, hacia el registro.
    heard: Sender<(AudioTrack, AudioTimeline)>,
    /// Contadores y sincronía, cada [`PUBLISH_TICKS`].
    snapshots: Sender<Snapshot>,
    /// Pares captura/presentación de los fotogramas, desde el registro (sincronía A/V).
    video: Receiver<(Instant, Instant)>,
    /// Emisores preparados (buffer y decodificador ya reservados) para el primero que llegue.
    spare_game: Receiver<Stream>,
    spare_voices: Receiver<Stream>,
    /// Emisores olvidados, para soltarlos fuera del tiempo real.
    retired: Sender<Stream>,
}

/// Los otros extremos de [`Links`], en el hilo de contadores.
struct HousekeeperLinks {
    heard: Receiver<(AudioTrack, AudioTimeline)>,
    snapshots: Receiver<Snapshot>,
    video: Sender<(Instant, Instant)>,
    spare_game: Sender<Stream>,
    spare_voices: Sender<Stream>,
    retired: Receiver<Stream>,
}

/// Marcas en vuelo hacia el registro (17 emisores × 2 tramas de holgura y de sobra para un
/// parón del hilo de contadores).
const HEARD_QUEUE: usize = 512;

/// Emisores de voz preparados de antemano: los que pueden empezar a hablar entre dos pasadas del
/// hilo de contadores.
const SPARE_VOICES: usize = 2;

fn links() -> (Links, HousekeeperLinks) {
    let (heard_tx, heard_rx) = crossbeam_channel::bounded(HEARD_QUEUE);
    let (snapshots_tx, snapshots_rx) = crossbeam_channel::bounded(2);
    let (video_tx, video_rx) = crossbeam_channel::bounded(coroom_audio::sync::VIDEO_WINDOW);
    let (spare_game_tx, spare_game_rx) = crossbeam_channel::bounded(1);
    let (spare_voices_tx, spare_voices_rx) = crossbeam_channel::bounded(SPARE_VOICES);
    let (retired_tx, retired_rx) = crossbeam_channel::bounded(MAX_VOICES + 1);
    (
        Links {
            heard: heard_tx,
            snapshots: snapshots_tx,
            video: video_rx,
            spare_game: spare_game_rx,
            spare_voices: spare_voices_rx,
            retired: retired_tx,
        },
        HousekeeperLinks {
            heard: heard_rx,
            snapshots: snapshots_rx,
            video: video_tx,
            spare_game: spare_game_tx,
            spare_voices: spare_voices_tx,
            retired: retired_rx,
        },
    )
}

/// Guarda un paquete en el buffer de su emisor. Un emisor nuevo toma uno de los preparados por el
/// hilo de contadores (si no queda ninguno, el paquete se pierde hasta la pasada siguiente).
fn accept(links: &Links, packet: Packet, game: &mut Option<Stream>, voices: &mut Vec<Stream>) {
    let stream = match packet.track {
        AudioTrack::Game => {
            if game.is_none() {
                let Ok(mut stream) = links.spare_game.try_recv() else {
                    return;
                };
                stream.source = packet.source;
                *game = Some(stream);
            }
            game.as_mut()
        }
        AudioTrack::Voice => {
            if !voices.iter().any(|v| v.source == packet.source) {
                if voices.len() >= MAX_VOICES {
                    return;
                }
                let Ok(mut stream) = links.spare_voices.try_recv() else {
                    return;
                };
                stream.source = packet.source;
                voices.push(stream);
            }
            voices.iter_mut().find(|v| v.source == packet.source)
        }
    };
    if let Some(stream) = stream {
        stream.last_packet = packet.arrived;
        stream.jitter.push_frames(
            packet.header.seq,
            packet.header.capture_us,
            packet.header.flags,
            packet.header.frames,
            &packet.data[..usize::from(packet.len)],
            packet.arrived,
        );
    }
}

/// La reproducción trama a trama: buffers, decodificación, mezcla, marcas y sincronía. La mueve
/// el hilo `audio-play` con la cola del sumidero o, a demanda, el propio dispositivo desde su
/// hilo de tiempo real: nada de lo que hace asigna memoria, ordena ni toma los cerrojos del
/// registro (todo eso va por [`Links`] al [`Housekeeper`]).
struct Playout {
    control: AudioControl,
    render: Option<RenderFeed>,
    packets: Receiver<Packet>,
    to_local: Box<ToLocal>,
    /// Ancla (marca de captura, instante local) con la que se convierten las marcas entre dos
    /// renovaciones, y la trama en que se renovó.
    anchor: Option<(u32, Instant, u32)>,
    channels: u16,
    mixer: Mixer,
    game: Option<Stream>,
    voices: Vec<Stream>,
    heard: Vec<Heard>,
    av: Option<AvSync>,
    ticks: u32,
    links: Links,
    /// Se cerró el canal de paquetes: la sesión termina.
    closed: Arc<AtomicBool>,
}

impl Playout {
    /// Llena `out` (una trama) con lo que toca en `now`, con lo más reciente que haya llegado.
    /// `false` si el canal de paquetes se cerró.
    fn fill(&mut self, out: &mut [i16], now: Instant) -> bool {
        loop {
            match self.packets.try_recv() {
                Ok(packet) => accept(&self.links, packet, &mut self.game, &mut self.voices),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.closed.store(true, Ordering::Release);
                    return false;
                }
            }
        }
        self.mixer.begin();
        self.heard.clear();
        let game_muted = self.control.game_muted();
        if let Some(stream) = &mut self.game
            && let Some((offset, info)) = stream.tick(now)
        {
            if !game_muted {
                // Su volumen en este equipo (el mezclador lo aplica al sumar; solo se le anota
                // cuando cambia).
                let gain = self.control.game_gain();
                if self.mixer.gain(GAME_SOURCE) != gain {
                    self.mixer.set_gain(GAME_SOURCE, gain);
                }
                let len = stream.frame_len();
                self.mixer.add(
                    GAME_SOURCE,
                    &stream.pcm[offset..offset + len],
                    stream.channels,
                );
            }
            self.heard.push(info);
        }
        for stream in &mut self.voices {
            if let Some((offset, info)) = stream.tick(now) {
                if self.control.voice_muted(stream.source) {
                    // Silenciada aquí: sigue su ritmo (para volver al instante), pero no suena.
                    continue;
                }
                // Su volumen en este equipo (el mezclador lo aplica al sumar; solo se le anota
                // cuando cambia, para no gastar sus entradas con voces al volumen normal).
                let gain = self.control.voice_gain(stream.source);
                if self.mixer.gain(stream.source) != gain {
                    self.mixer.set_gain(stream.source, gain);
                }
                let len = stream.frame_len();
                self.mixer.add(
                    stream.source,
                    &stream.pcm[offset..offset + len],
                    stream.channels,
                );
                self.heard.push(info);
            }
        }
        self.mixer.finish(out);
        true
    }

    /// Sin salida que funcione: tira lo que llegó (sin decodificarlo), para que al volver a sonar
    /// no quede una cola vieja. `false` si el canal de paquetes se cerró.
    fn discard(&mut self) -> bool {
        loop {
            match self.packets.try_recv() {
                Ok(_) => {}
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => {
                    self.closed.store(true, Ordering::Release);
                    return false;
                }
            }
        }
    }

    /// Instante local de una marca de captura (reloj del anfitrión). La conversión de la sesión
    /// (`to_local`) pasa por el cerrojo del reloj del enlace: se llama una vez cada
    /// [`SYNC_TICKS`] tramas para renovar el ancla y las demás marcas se sitúan respecto a ella
    /// (las dos van en el mismo reloj; el desfase estimado cambia despacio).
    fn local_capture(&mut self, capture_us: u32) -> Option<Instant> {
        let stale = self
            .anchor
            .is_none_or(|(_, _, at_tick)| self.ticks.wrapping_sub(at_tick) >= SYNC_TICKS);
        if stale {
            let anchored = (self.to_local)(capture_us)?;
            self.anchor = Some((capture_us, anchored, self.ticks));
            return Some(anchored);
        }
        let (anchor_us, anchored, _) = self.anchor?;
        let delta = capture_us.wrapping_sub(anchor_us) as i32;
        if delta >= 0 {
            anchored.checked_add(Duration::from_micros(u64::from(delta.unsigned_abs())))
        } else {
            anchored.checked_sub(Duration::from_micros(u64::from(delta.unsigned_abs())))
        }
    }

    /// `out`, que [`Playout::fill`] llenó en `decided`, sonará en `played_at`: marcas, sincronía
    /// (cada [`SYNC_TICKS`] tramas) y, cada [`PUBLISH_TICKS`], la foto de los contadores.
    fn played(&mut self, out: &[i16], decided: Instant, played_at: Instant) {
        if let Some(render) = &mut self.render {
            render.feed_render(out, self.channels);
        }

        // Marcas de lo que sonó (las tramas reales; lo ocultado no tiene captura).
        for index in 0..self.heard.len() {
            let info = self.heard[index];
            let Some(capture) = info.capture_us.and_then(|us| self.local_capture(us)) else {
                continue;
            };
            let mut timeline = AudioTimeline::default();
            timeline.mark(AudioStage::Captured, capture);
            if let Some(arrived) = info.arrived {
                timeline.mark(AudioStage::Received, arrived);
            }
            timeline.mark(AudioStage::Decoded, decided);
            timeline.mark(AudioStage::Played, played_at);
            // Lleno solo si el hilo de contadores lleva parado un rato: se pierde la marca.
            let _ = self.links.heard.try_send((info.track, timeline));
            if info.track == AudioTrack::Game
                && let Some(av) = &mut self.av
            {
                av.on_audio(capture, played_at);
            }
        }

        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(SYNC_TICKS)
            && let Some(av) = &mut self.av
        {
            while let Ok((captured, presented)) = self.links.video.try_recv() {
                av.on_video(captured, presented);
            }
            if let Some(stream) = &mut self.game {
                let (min, max) = stream.jitter.extra_delay_range_ms();
                av.set_limits(min, max);
                let adjust = av.recommend();
                stream.jitter.set_extra_delay(adjust);
            }
        }
        if self.ticks.is_multiple_of(PUBLISH_TICKS) {
            // Si el hilo de contadores no recogió la anterior, esta se pierde (la siguiente sí).
            let _ = self.links.snapshots.try_send(self.snapshot());
            self.retire_idle(decided);
        }
    }

    /// Contadores de los buffers y copia de la sincronía, sin asignar.
    fn snapshot(&self) -> Snapshot {
        let mut snapshot = Snapshot {
            streams: [(AudioTrack::Game, JitterStats::default()); MAX_VOICES + 1],
            len: 0,
            av: self.av.clone(),
        };
        for stream in self.game.iter().chain(self.voices.iter()) {
            if snapshot.len < snapshot.streams.len() {
                snapshot.streams[snapshot.len] = (stream.track, stream.jitter.stats());
                snapshot.len += 1;
            }
        }
        snapshot
    }

    /// Olvida las voces que llevan [`IDLE_SOURCE`] sin enviar: se sueltan en el hilo de contadores.
    fn retire_idle(&mut self, now: Instant) {
        let mut index = 0;
        while index < self.voices.len() {
            if now.saturating_duration_since(self.voices[index].last_packet) < IDLE_SOURCE {
                index += 1;
                continue;
            }
            let stream = self.voices.swap_remove(index);
            if let Err(err) = self.links.retired.try_send(stream) {
                // Sin sitio: se queda hasta la próxima vez (el orden de la mezcla da igual).
                self.voices.push(err.into_inner());
                index += 1;
            }
        }
    }
}

/// El hilo de contadores de la reproducción: lo que no puede hacerse en tiempo real. Pasa las
/// marcas y los contadores al registro, calcula los percentiles de la sincronía, lleva al
/// reproductor los fotogramas que necesita la sincronía, prepara los emisores nuevos y suelta
/// los olvidados.
struct Housekeeper {
    core: Arc<Core>,
    control: AudioControl,
    links: HousekeeperLinks,
    av_sync: bool,
    last_video: u64,
    last_status: Option<Instant>,
    device: Arc<DeviceStats>,
}

/// Cada cuánto se publica quién habla.
const STATUS_PERIOD: Duration = Duration::from_millis(500);

impl Housekeeper {
    /// Sin bloquear: lo pendiente de cada canal.
    fn tick(&mut self) {
        let ledger = &self.core.ledger;
        self.refill();
        while let Ok(stream) = self.links.retired.try_recv() {
            drop(stream);
        }
        if self.av_sync {
            let video = &self.links.video;
            ledger.video_pairs_since(&mut self.last_video, |captured, presented| {
                // Lleno: el reproductor no está sonando el juego; esos fotogramas no hacen falta.
                let _ = video.try_send((captured, presented));
            });
        }
        while let Ok((track, timeline)) = self.links.heard.try_recv() {
            ledger.record_audio(track, AudioSide::Receiver, timeline);
        }
        while let Ok(snapshot) = self.links.snapshots.try_recv() {
            self.publish(&snapshot);
        }
        let now = Instant::now();
        if self
            .last_status
            .is_none_or(|last| now.saturating_duration_since(last) >= STATUS_PERIOD)
        {
            self.last_status = Some(now);
            let speaking = self
                .control
                .talkers()
                .into_iter()
                .map(talker_label)
                .collect();
            ledger.set_voice_status(speaking, self.control.status_label());
        }
    }

    /// Deja preparados un emisor del juego y [`SPARE_VOICES`] de voz.
    fn refill(&self) {
        let fill = |track: AudioTrack, spares: &Sender<Stream>| {
            while !spares.is_full() {
                match Stream::new(track, 0) {
                    Ok(stream) => {
                        if spares.try_send(stream).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        self.core.warn(format!("audio: {err}"));
                        break;
                    }
                }
            }
        };
        fill(AudioTrack::Game, &self.links.spare_game);
        fill(AudioTrack::Voice, &self.links.spare_voices);
    }

    fn publish(&self, snapshot: &Snapshot) {
        let ledger = &self.core.ledger;
        if let Some(av) = &snapshot.av {
            let stats = av.stats();
            ledger.set_av(AvSummary {
                samples: stats.samples,
                p50_ms: stats.p50_abs_ms,
                p95_ms: stats.p95_abs_ms,
                offset_ms: stats.offset_ms,
                adjust_ms: stats.adjust_ms,
            });
        }
        publish(ledger, snapshot.streams(), self.device.load());
    }

    /// Al terminar: lo último que quedó en los canales y la foto final de la reproducción.
    fn finish(&mut self, playout: &Playout) {
        self.tick();
        self.publish(&playout.snapshot());
    }
}

// La foto de los contadores cae en una trama de sincronía.
const _: () = assert!(PUBLISH_TICKS.is_multiple_of(SYNC_TICKS));

/// Hilo `audio-play`. Con un sumidero a demanda ([`AudioSink::start_pull`]) el dispositivo pide
/// cada trama justo cuando la necesita y este hilo solo vigila y hace de hilo de contadores; si
/// no, la decide este hilo en cuanto el sumidero tiene sitio, una trama antes de que suene, y los
/// contadores van en un hilo aparte (`audio-stats`). Si la salida no abre o deja de funcionar,
/// avisa una vez ([`AudioIssue`]), tira lo que llega y espera a que se elija otra
/// ([`AudioControl::replace_speaker`]); solo termina con la sesión.
pub(crate) fn play_loop(
    core: &Arc<Core>,
    player: Player,
    packets: Receiver<Packet>,
    to_local: impl Fn(u32) -> Option<Instant> + Send + Sync + 'static,
) {
    let Player {
        mut sink,
        control,
        render,
        av_sync,
    } = player;
    let format = sink.format();
    let config = AudioConfig {
        format,
        device: None,
        latency: coroom_audio::DEVICE_LATENCY,
    };
    *lock(&control.inner.speaker_format) = Some(format);
    let closed = Arc::new(AtomicBool::new(false));
    let device = Arc::new(DeviceStats::default());
    let (links, housekeeper_links) = links();
    let housekeeper = Housekeeper {
        core: Arc::clone(core),
        control: control.clone(),
        links: housekeeper_links,
        av_sync,
        last_video: 0,
        last_status: None,
        device: Arc::clone(&device),
    };
    // Los emisores del arranque, preparados antes de que suene nada.
    housekeeper.refill();
    let playout = Playout {
        control,
        render,
        packets,
        to_local: Box::new(to_local),
        anchor: None,
        channels: format.channels,
        mixer: Mixer::new(format.channels),
        game: None,
        voices: Vec::with_capacity(MAX_VOICES),
        heard: Vec::with_capacity(MAX_VOICES + 1),
        av: av_sync.then(AvSync::new),
        ticks: 0,
        links,
        closed: Arc::clone(&closed),
    };
    let frame_len = coroom_audio::SAMPLES_PER_CHANNEL * usize::from(format.channels);
    let speaker_control = housekeeper.control.clone();
    if sink.supports_pull() {
        play_pull(core, &mut sink, &config, playout, housekeeper, &closed);
    } else {
        play_push(core, &mut sink, &config, playout, housekeeper, frame_len);
    }
    *lock(&speaker_control.inner.speaker_format) = None;
}

/// Adopta la salida nueva pedida en marcha ([`AudioControl::replace_speaker`]) y le contesta si
/// abrió: suelta la actual (si funcionaba) y `restart` abre la nueva. Si no abre, se vuelve a
/// abrir la anterior (si funcionaba; si tampoco abre, se avisa). `live` dice si la salida en uso
/// funciona.
fn swap_speaker(
    core: &Core,
    control: &AudioControl,
    sink: &mut Box<dyn AudioSink>,
    live: &mut bool,
    mut restart: impl FnMut(&mut dyn AudioSink) -> Result<(), AudioError>,
) {
    let Some((mut next, reply)) = control.inner.new_speaker.take() else {
        return;
    };
    if next.supports_pull() != sink.supports_pull() {
        let _ = reply.try_send(Err(DeviceSwitchError::Incompatible));
        return;
    }
    if *live {
        sink.stop();
    }
    match restart(next.as_mut()) {
        Ok(()) => {
            debug!(salida = %next.describe(), "salida cambiada en marcha");
            *sink = next;
            *live = true;
            let _ = reply.try_send(Ok(()));
        }
        Err(err) => {
            let _ = reply.try_send(Err(DeviceSwitchError::Open(err)));
            if *live && let Err(err) = restart(sink.as_mut()) {
                report(core, AudioIssue::SpeakerLost(err));
                *live = false;
            }
        }
    }
}

/// La salida dejó de funcionar: se avisa una vez, se suelta y se espera otra.
fn lose_speaker(core: &Core, sink: &mut dyn AudioSink, live: &mut bool, err: AudioError) {
    report(core, AudioIssue::SpeakerLost(err));
    sink.stop();
    *live = false;
}

/// A demanda: el dispositivo llama a la reproducción desde su hilo; este, el de contadores.
fn play_pull(
    core: &Core,
    sink: &mut Box<dyn AudioSink>,
    config: &AudioConfig,
    playout: Playout,
    mut housekeeper: Housekeeper,
    closed: &AtomicBool,
) {
    // Solo el dispositivo lo toma mientras suena (este hilo, al final): el cerrojo no se disputa
    // (salvo un instante al cambiar de salida, con la anterior ya parada).
    let playout = Arc::new(Mutex::new(playout));
    let render = || -> Render {
        let device = Arc::clone(&playout);
        Box::new(move |out, played_at| {
            let mut playout = lock(&device);
            let decided = Instant::now();
            if playout.fill(out, decided) {
                playout.played(out, decided, played_at);
            } else {
                out.fill(0);
            }
        })
    };
    let mut live = match sink.start_pull(config, render()) {
        Ok(()) => true,
        Err(err) => {
            report(core, AudioIssue::SpeakerUnavailable(err));
            false
        }
    };
    debug!(salida = %sink.describe(), "reproducción de audio a demanda");
    while !core.shared.stopping() && !closed.load(Ordering::Acquire) {
        swap_speaker(core, &housekeeper.control, sink, &mut live, |next| {
            next.start_pull(config, render())
        });
        if live && let Err(err) = sink.check() {
            lose_speaker(core, sink.as_mut(), &mut live, err);
        }
        if !live {
            // Sin salida que funcione, lo que llega se tira: al volver suena lo de ahora.
            lock(&playout).discard();
        }
        housekeeper.device.store(sink.device_counters());
        housekeeper.tick();
        std::thread::sleep(PULL_WATCH);
    }
    housekeeper.device.store(sink.device_counters());
    sink.stop();
    housekeeper.finish(&lock(&playout));
}

/// Con la cola del sumidero: se espera su sitio y después se decide qué suena.
fn play_push(
    core: &Core,
    sink: &mut Box<dyn AudioSink>,
    config: &AudioConfig,
    mut playout: Playout,
    mut housekeeper: Housekeeper,
    frame_len: usize,
) {
    let mut live = match sink.start(config) {
        Ok(()) => true,
        Err(err) => {
            report(core, AudioIssue::SpeakerUnavailable(err));
            false
        }
    };
    debug!(salida = %sink.describe(), "reproducción de audio");
    let device = Arc::clone(&housekeeper.device);
    let control = housekeeper.control.clone();
    let done_flag = AtomicBool::new(false);
    let done = &done_flag;
    std::thread::scope(|scope| {
        let stats = std::thread::Builder::new()
            .name("audio-stats".into())
            .spawn_scoped(scope, move || {
                while !done.load(Ordering::Acquire) {
                    housekeeper.tick();
                    std::thread::sleep(PULL_WATCH);
                }
                housekeeper
            });
        let mut stats = match stats {
            Ok(handle) => Some(handle),
            Err(err) => {
                core.warn(format!("sin contadores de audio: {err}"));
                None
            }
        };
        // Con la cola, este hilo marca el ritmo del sumidero (a demanda lo marca el dispositivo).
        realtime("audio-play");
        let mut out = [0i16; MAX_FRAME_SAMPLES];
        while !core.shared.stopping() {
            swap_speaker(core, &control, sink, &mut live, |next| next.start(config));
            if !live {
                // Sin salida que funcione, lo que llega se tira hasta que se elija otra.
                if !playout.discard() {
                    break;
                }
                std::thread::sleep(DEAD_DEVICE_POLL);
                continue;
            }
            if let Err(err) = sink.wait_ready() {
                lose_speaker(core, sink.as_mut(), &mut live, err);
                continue;
            }
            let decided = Instant::now();
            if !playout.fill(&mut out[..frame_len], decided) {
                break;
            }
            let played = match sink.write(&out[..frame_len]) {
                Ok(played) => played,
                Err(err) => {
                    lose_speaker(core, sink.as_mut(), &mut live, err);
                    continue;
                }
            };
            device.store(sink.device_counters());
            playout.played(&out[..frame_len], decided, played.played_at_estimate);
        }
        device.store(sink.device_counters());
        done.store(true, Ordering::Release);
        if let Some(mut housekeeper) = stats.take().and_then(|handle| handle.join().ok()) {
            housekeeper.finish(&playout);
        }
    });
    sink.stop();
}

/// Sube el hilo de audio que llama a tiempo real ([`coroom_audio::realtime`]); si no se concede,
/// sigue con prioridad normal.
fn realtime(thread: &str) {
    match coroom_audio::realtime::promote_current_thread() {
        Ok(via) => debug!(hilo = thread, via, "hilo de audio en tiempo real"),
        Err(reason) => debug!(hilo = thread, %reason, "hilo de audio sin tiempo real"),
    }
}

/// Espera máxima de `audio-net` antes de mirar si la sesión terminó.
const READER_POLL: Duration = Duration::from_millis(100);

/// Hilo `audio-net`: lee los datagramas de audio de una conexión y los pasa a `handle` (SFrame,
/// cabecera y canal hacia `audio-play`) en un hilo propio con prioridad de tiempo real, fuera del
/// runtime de la red, que comparte sus hilos con el vídeo, QUIC y el control: así un fotograma
/// clave grande no retrasa el audio. Termina al cerrarse la conexión o la sesión, o con
/// [`AudioReader::stop`].
pub(crate) struct AudioReader {
    stop: Arc<AtomicBool>,
}

impl AudioReader {
    pub(crate) fn spawn(
        core: &Arc<Core>,
        connection: PeerConnection,
        mut handle: impl FnMut(Bytes) + Send + 'static,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_stop, shared) = (Arc::clone(&stop), Arc::clone(&core.shared));
        std::thread::Builder::new()
            .name("audio-net".into())
            .spawn(move || {
                realtime("audio-net");
                while !thread_stop.load(Ordering::Acquire) && !shared.stopping() {
                    match connection.read_audio_datagram_blocking(READER_POLL) {
                        Ok(Some(datagram)) => handle(datagram),
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
            })
            .map_err(|err| format!("no se pudo crear `audio-net`: {err}"))?;
        Ok(Self { stop })
    }

    /// Lo para (sin esperarlo: sale en menos de [`READER_POLL`]).
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Suma los contadores de los buffers por pista y los deja en el registro, con los de la salida
/// (común a las pistas: el mismo valor en cada una).
fn publish(
    ledger: &LatencyLedger,
    streams: impl Iterator<Item = (AudioTrack, JitterStats)>,
    device: DeviceCounters,
) {
    let mut totals = [AudioCounters {
        device_underruns: device.underruns,
        device_overruns: device.overruns,
        ..AudioCounters::default()
    }; 2];
    let mut seen = [false; 2];
    for (track, stats) in streams {
        let index = match track {
            AudioTrack::Game => 0,
            AudioTrack::Voice => 1,
        };
        seen[index] = true;
        let total = &mut totals[index];
        total.received += stats.received;
        total.played += stats.played;
        total.concealed += stats.concealed;
        total.recovered_fec += stats.recovered_fec;
        total.recovered_dred += stats.recovered_dred;
        total.late += stats.late;
        total.underruns += stats.underruns;
        total.trimmed += stats.trimmed;
        total.stretched += stats.stretched;
        total.max_gap_frames = total.max_gap_frames.max(stats.max_gap_frames);
        total.jitter_target_ms = total.jitter_target_ms.max(stats.target_ms);
        total.jitter_p95_ms = total.jitter_p95_ms.max(stats.jitter_p95_ms);
    }
    for (track, (counters, seen)) in AudioTrack::ALL
        .into_iter()
        .zip(totals.into_iter().zip(seen))
    {
        if seen {
            ledger.set_audio_counters(track, counters);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coroom_audio::synthetic::{CaptureSink, ToneSource};

    #[test]
    fn microseconds_extend_around_the_reference() {
        assert_eq!(extend_us(5_000_000, 4_990_000), 4_990_000);
        // Después de dar la vuelta a los 32 bits.
        let reference = (1u64 << 32) + 1_000;
        assert_eq!(extend_us(reference, u32::MAX - 999), (1u64 << 32) - 1_000);
        // Un poco en el futuro también vale.
        assert_eq!(extend_us(1_000, 2_000), 2_000);
        assert_eq!(extend_us(0, u32::MAX), 0, "nunca negativo");
    }

    #[test]
    fn the_control_reads_in_spanish() {
        let control = AudioControl::default();
        assert_eq!(control.status_label(), "sin micrófono");
        control.set_voice_enabled(true);
        assert_eq!(control.status_label(), "sin permiso de voz");
        control.set_voice_allowed(true);
        assert_eq!(
            control.status_label(),
            "micrófono abierto (activación por voz)"
        );
        assert!(control.toggle_mic());
        assert_eq!(control.status_label(), "micrófono silenciado");
        assert!(!control.toggle_mic());
        control.set_push_to_talk(true);
        assert_eq!(control.status_label(), "pulsar para hablar");
        assert!(control.toggle_game());
        assert!(control.status_label().ends_with("juego silenciado"));
        control.set_talker(3, true);
        control.set_talker(0, true);
        assert_eq!(control.talkers(), vec![0, 3]);
        control.set_talker(3, false);
        assert_eq!(talker_label(control.talkers()[0]), "anfitrión");
        assert_eq!(talker_label(2), "invitado 2");
        assert!(control.set_speaking(true) && !control.set_speaking(true));
    }

    #[test]
    fn muted_voices_live_in_atomic_slots() {
        let control = AudioControl::default();
        assert!(!control.voice_muted(7));
        control.set_voice_muted(7, true);
        control.set_voice_muted(7, true);
        control.set_voice_muted(u32::MAX, true);
        assert!(control.voice_muted(7) && control.voice_muted(u32::MAX));
        assert!(
            !control.voice_muted(0),
            "el emisor 0 no se confunde con un hueco libre"
        );
        control.set_voice_muted(0, true);
        assert!(control.voice_muted(0));
        control.set_voice_muted(7, false);
        assert!(!control.voice_muted(7) && control.voice_muted(0));
        // Un solo hueco por emisor: silenciar dos veces y quitar una vez lo deja libre.
        let used = control
            .inner
            .muted_voices
            .iter()
            .filter(|slot| slot.load(Ordering::Relaxed) != 0)
            .count();
        assert_eq!(used, 2);
        // Lleno: los que no caben no se silencian, y al liberar vuelve a haber sitio.
        for source in 100..100 + MUTED_SLOTS as u32 {
            control.set_voice_muted(source, true);
        }
        assert!(!control.voice_muted(100 + MUTED_SLOTS as u32 - 2));
        control.set_voice_muted(0, false);
        control.set_voice_muted(999, true);
        assert!(control.voice_muted(999));
    }

    #[test]
    fn voice_gains_live_in_atomic_slots() {
        let control = AudioControl::default();
        assert_eq!(control.voice_gain(7), 1.0, "sin tocar, volumen normal");
        control.set_voice_gain(7, 2.0);
        control.set_voice_gain(u32::MAX, 0.5);
        assert_eq!(
            (control.voice_gain(7), control.voice_gain(u32::MAX)),
            (2.0, 0.5)
        );
        assert_eq!(
            control.voice_gain(0),
            1.0,
            "el emisor 0 no se confunde con un hueco libre"
        );
        control.set_voice_gain(0, 0.0);
        assert_eq!(control.voice_gain(0), 0.0, "ni tampoco al volumen cero");
        // Acotada a 0.0..=2.0; lo que no es un número cuenta como 1.0.
        control.set_voice_gain(9, 5.0);
        assert_eq!(control.voice_gain(9), MAX_VOICE_GAIN);
        control.set_voice_gain(9, -1.0);
        assert_eq!(control.voice_gain(9), 0.0);
        control.set_voice_gain(9, f32::NAN);
        assert_eq!(control.voice_gain(9), 1.0);
        // Un solo hueco por emisor, que cambia en el sitio y se libera al volver a 1.0.
        let used = || {
            control
                .inner
                .voice_gains
                .iter()
                .filter(|slot| slot.load(Ordering::Relaxed) != 0)
                .count()
        };
        assert_eq!(used(), 3);
        control.set_voice_gain(7, 1.5);
        assert_eq!((control.voice_gain(7), used()), (1.5, 3));
        control.set_voice_gain(7, 1.0);
        assert_eq!((control.voice_gain(7), used()), (1.0, 2));
        // Lleno: los que no caben suenan al volumen normal, y al liberar vuelve a haber sitio.
        for source in 100..100 + GAIN_SLOTS as u32 {
            control.set_voice_gain(source, 0.25);
        }
        assert_eq!(control.voice_gain(100 + GAIN_SLOTS as u32 - 2), 1.0);
        control.set_voice_gain(0, 1.0);
        control.set_voice_gain(999, 0.75);
        assert_eq!(control.voice_gain(999), 0.75);
        // El silencio va aparte.
        control.set_voice_muted(999, true);
        assert!(control.voice_muted(999));
        assert_eq!(control.voice_gain(999), 0.75);
    }

    #[test]
    fn the_mic_gain_and_device_swaps_wait_for_their_thread() {
        let control = AudioControl::default();
        assert_eq!(control.mic_gain(), 1.0, "de partida, lo normal");
        control.set_mic_gain(1.5);
        assert_eq!(control.mic_gain(), 1.5);
        control.set_mic_gain(9.0);
        assert_eq!(control.mic_gain(), MAX_VOICE_GAIN, "se acota");
        control.set_mic_gain(f32::NAN);
        assert_eq!(control.mic_gain(), 1.0, "no finita cuenta como normal");
        // Sin hilo de la voz ni reproducción, cambiar de dispositivo lo dice (y no deja nada
        // pendiente).
        assert_eq!(
            control.replace_mic(Box::new(ToneSource::new(440.0, 1))),
            Err(DeviceSwitchError::NotRunning)
        );
        assert!(control.inner.new_mic.take().is_none());
        assert_eq!(control.speaker_format(), None, "nada que imitar");
        assert_eq!(
            control.replace_speaker(Box::new(CaptureSink::new(2))),
            Err(DeviceSwitchError::NotRunning)
        );
        assert!(control.inner.new_speaker.take().is_none());
    }

    #[test]
    fn a_zero_voice_volume_counts_as_muted() {
        let control = AudioControl::default();
        control.set_voice_allowed(true);
        assert!(sends_voice(&control, true));
        assert!(!sends_voice(&control, false), "el VAD no oye nada");
        control.set_mic_gain(0.0);
        assert!(
            !sends_voice(&control, true),
            "a cero no se envía ni se habla"
        );
        control.set_mic_gain(0.05);
        assert!(sends_voice(&control, true));
        control.set_push_to_talk(true);
        assert!(!sends_voice(&control, true), "sin la tecla pulsada");
        control.set_talk_pressed(true);
        assert!(sends_voice(&control, false));
        control.set_mic_gain(0.0);
        assert!(!sends_voice(&control, false), "ni con la tecla pulsada");
        control.set_mic_gain(1.0);
        control.set_mic_muted(true);
        assert!(!sends_voice(&control, true));
        control.set_mic_muted(false);
        control.set_voice_allowed(false);
        assert!(!sends_voice(&control, true));
    }

    /// Micrófono sintético que deja de funcionar (como uno USB que se desconecta): `start` falla
    /// si se le pide y `read` falla tras `frames` tramas.
    struct FlakyMic {
        tone: ToneSource,
        fail_start: bool,
        frames: u32,
        read: Arc<AtomicU32>,
    }

    impl FlakyMic {
        fn new(fail_start: bool, frames: u32) -> Self {
            Self {
                tone: ToneSource::new(440.0, 1),
                fail_start,
                frames,
                read: Arc::new(AtomicU32::new(0)),
            }
        }
    }

    impl AudioSource for FlakyMic {
        fn format(&self) -> AudioFormat {
            self.tone.format()
        }

        fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
            if self.fail_start {
                return Err(AudioError::NoSuchDevice("Micrófono de ejemplo".into()));
            }
            self.tone.start(config)
        }

        fn read(&mut self, out: &mut Vec<i16>) -> Result<coroom_audio::Captured, AudioError> {
            if self.read.fetch_add(1, Ordering::AcqRel) >= self.frames {
                return Err(AudioError::DeviceLost);
            }
            self.tone.read(out)
        }

        fn stop(&mut self) {
            self.tone.stop();
        }
    }

    /// Destino de prueba: cuenta los paquetes de voz enviados.
    #[derive(Default)]
    struct CountingOut {
        sent: AtomicU32,
    }

    impl AudioOut for CountingOut {
        fn send(&self, _track: Track, _seq: u32, _sealed: &[u8]) -> bool {
            self.sent.fetch_add(1, Ordering::AcqRel);
            true
        }

        fn has_targets(&self, _track: Track) -> bool {
            true
        }

        fn host_us(&self, _at: Instant) -> Option<u64> {
            Some(0)
        }

        fn speaking(&self, _speaking: bool) {}
    }

    fn test_core() -> Arc<Core> {
        Arc::new(Core::new(
            crate::Role::Host,
            coroom_transport::crypto::Identity::generate().expect("identidad"),
            Arc::new(LatencyLedger::default()),
            false,
        ))
    }

    /// Los avisos de audio que la sesión emitió hasta ahora.
    fn issues(core: &Core) -> Vec<AudioIssue> {
        core.events_rx
            .try_iter()
            .filter_map(|event| match event {
                SessionEvent::Audio(issue) => Some(issue),
                _ => None,
            })
            .collect()
    }

    fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition() {
            assert!(Instant::now() < deadline, "tiempo agotado: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Arranca `voice-cap` con `mic`, pulsar para hablar y la tecla pulsada (envía cada trama).
    fn spawn_voice(
        core: &Arc<Core>,
        mic: Box<dyn AudioSource>,
    ) -> (Arc<CountingOut>, std::thread::JoinHandle<()>) {
        let control = core.audio.clone();
        control.set_voice_enabled(true);
        control.set_voice_allowed(true);
        control.set_push_to_talk(true);
        control.set_talk_pressed(true);
        let out = Arc::new(CountingOut::default());
        let (thread_core, thread_out) = (Arc::clone(core), Arc::clone(&out));
        let processor = VoiceProcessor::new(ProcessingOptions::NONE).expect("procesado");
        let handle = std::thread::spawn(move || {
            let input = VoiceInput::new(mic);
            voice_capture_loop(
                &thread_core,
                input,
                processor,
                10,
                &thread_core.audio,
                thread_out.as_ref(),
            );
        });
        (out, handle)
    }

    #[test]
    fn a_lost_mic_waits_for_another_one_instead_of_ending_the_voice() {
        let core = test_core();
        let control = core.audio.clone();
        let (out, handle) = spawn_voice(&core, Box::new(FlakyMic::new(false, 20)));
        // Deja de funcionar: un aviso, sin voz, y el hilo sigue esperando otro.
        wait_for("el aviso", || !control.voice_enabled());
        let sent = out.sent.load(Ordering::Acquire);
        assert!(sent > 0 && sent <= 20, "{sent}");
        assert_eq!(issues(&core), [AudioIssue::MicLost(AudioError::DeviceLost)]);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            out.sent.load(Ordering::Acquire),
            sent,
            "sin micrófono no sale nada"
        );
        assert!(issues(&core).is_empty(), "se avisa una sola vez");
        assert!(control.inner.mic_running.load(Ordering::Acquire));
        // Un micrófono que no abre no se adopta: lo dice y se sigue esperando.
        assert_eq!(
            control.replace_mic(Box::new(FlakyMic::new(true, u32::MAX))),
            Err(DeviceSwitchError::Open(AudioError::NoSuchDevice(
                "Micrófono de ejemplo".into()
            )))
        );
        // Otro que funciona: la voz vuelve sin reconectar.
        assert_eq!(
            control.replace_mic(Box::new(ToneSource::new(550.0, 1))),
            Ok(())
        );
        assert!(control.voice_enabled());
        wait_for("la voz de nuevo", || {
            out.sent.load(Ordering::Acquire) > sent + 5
        });
        core.shared.finish(None);
        handle.join().expect("voice-cap");
        assert!(!control.inner.mic_running.load(Ordering::Acquire));
        assert_eq!(
            control.replace_mic(Box::new(ToneSource::new(550.0, 1))),
            Err(DeviceSwitchError::NotRunning),
            "con la sesión terminada"
        );
    }

    #[test]
    fn a_mic_that_never_opened_can_be_replaced_in_the_room() {
        let core = test_core();
        let control = core.audio.clone();
        let (out, handle) = spawn_voice(&core, Box::new(FlakyMic::new(true, u32::MAX)));
        wait_for("el aviso", || !control.voice_enabled());
        wait_for("el hilo en marcha", || {
            control.inner.mic_running.load(Ordering::Acquire)
        });
        assert!(matches!(
            issues(&core).as_slice(),
            [AudioIssue::MicUnavailable(AudioError::NoSuchDevice(_))]
        ));
        assert_eq!(out.sent.load(Ordering::Acquire), 0);
        assert_eq!(
            control.replace_mic(Box::new(ToneSource::new(550.0, 1))),
            Ok(())
        );
        wait_for("la voz", || out.sent.load(Ordering::Acquire) > 5);
        assert!(control.voice_enabled());
        // A volumen cero, como silenciado: no sale nada más.
        control.set_mic_gain(0.0);
        wait_for("silenciado", || !control.speaking());
        let muted = out.sent.load(Ordering::Acquire);
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            out.sent.load(Ordering::Acquire) <= muted + 1,
            "como mucho la trama DTX del final"
        );
        core.shared.finish(None);
        handle.join().expect("voice-cap");
    }

    /// Salida sintética que deja de funcionar cuando se le dice (como una USB que se desconecta),
    /// o que nunca abre.
    struct FlakySpeaker {
        sink: CaptureSink,
        fail_start: bool,
        lost: Arc<AtomicBool>,
    }

    impl AudioSink for FlakySpeaker {
        fn format(&self) -> AudioFormat {
            self.sink.format()
        }

        fn start(&mut self, config: &AudioConfig) -> Result<(), AudioError> {
            if self.fail_start {
                return Err(AudioError::NoSuchDevice("Salida de ejemplo".into()));
            }
            self.sink.start(config)
        }

        fn wait_ready(&mut self) -> Result<(), AudioError> {
            if self.lost.load(Ordering::Acquire) {
                return Err(AudioError::DeviceLost);
            }
            self.sink.wait_ready()
        }

        fn write(&mut self, samples: &[i16]) -> Result<coroom_audio::Played, AudioError> {
            self.sink.write(samples)
        }

        fn supports_pull(&self) -> bool {
            self.sink.supports_pull()
        }

        fn start_pull(&mut self, config: &AudioConfig, render: Render) -> Result<(), AudioError> {
            if self.fail_start {
                return Err(AudioError::NoSuchDevice("Salida de ejemplo".into()));
            }
            self.sink.start_pull(config, render)
        }

        fn check(&mut self) -> Result<(), AudioError> {
            if self.lost.load(Ordering::Acquire) {
                return Err(AudioError::DeviceLost);
            }
            self.sink.check()
        }

        fn stop(&mut self) {
            self.sink.stop();
        }
    }

    /// Una salida que deja de funcionar no termina la reproducción: se avisa y se puede elegir
    /// otra (que se graba); una que no abre lo dice y no cambia nada. A demanda (`pull`) y con
    /// la cola del sumidero.
    fn a_lost_speaker_can_be_replaced(pull: bool, never_opened: bool) {
        let core = test_core();
        let control = core.audio.clone();
        let lost = Arc::new(AtomicBool::new(false));
        let sink = |fail_start: bool| {
            let sink = CaptureSink::new(2);
            FlakySpeaker {
                sink: if pull { sink } else { sink.pushed() },
                fail_start,
                lost: Arc::clone(&lost),
            }
        };
        let first = sink(never_opened);
        let (packets_tx, packets) = packet_channel();
        let thread_core = Arc::clone(&core);
        let player = Player {
            sink: Box::new(first),
            control: control.clone(),
            render: None,
            av_sync: false,
        };
        let handle = std::thread::spawn(move || {
            play_loop(&thread_core, player, packets, |_| None);
        });
        wait_for("la reproducción", || control.speaker_format().is_some());
        if never_opened {
            wait_for("el aviso", || {
                matches!(
                    core.events_rx.try_recv(),
                    Ok(SessionEvent::Audio(AudioIssue::SpeakerUnavailable(_)))
                )
            });
        } else {
            lost.store(true, Ordering::Release);
            wait_for("el aviso", || {
                matches!(
                    core.events_rx.try_recv(),
                    Ok(SessionEvent::Audio(AudioIssue::SpeakerLost(
                        AudioError::DeviceLost
                    )))
                )
            });
        }
        assert!(control.speaker_format().is_some(), "la reproducción sigue");
        lost.store(false, Ordering::Release);
        assert_eq!(
            control.replace_speaker(Box::new(sink(true))),
            Err(DeviceSwitchError::Open(AudioError::NoSuchDevice(
                "Salida de ejemplo".into()
            )))
        );
        let other = sink(false);
        let tape = other.sink.recording();
        assert_eq!(control.replace_speaker(Box::new(other)), Ok(()));
        wait_for("la salida nueva", || {
            tape.lock().map(|r| r.samples.len()).unwrap_or(0) > 4_800
        });
        assert!(issues(&core).is_empty(), "{:?}", issues(&core));
        // Con el canal de paquetes cerrado (la sesión termina), el hilo acaba y ya no hay salida
        // que cambiar.
        drop(packets_tx);
        core.shared.finish(None);
        handle.join().expect("audio-play");
        assert_eq!(control.speaker_format(), None);
        assert_eq!(
            control.replace_speaker(Box::new(sink(false))),
            Err(DeviceSwitchError::NotRunning)
        );
    }

    #[test]
    fn a_lost_speaker_can_be_replaced_on_demand() {
        a_lost_speaker_can_be_replaced(true, false);
    }

    #[test]
    fn a_lost_speaker_can_be_replaced_with_the_queue() {
        a_lost_speaker_can_be_replaced(false, false);
    }

    #[test]
    fn a_speaker_that_never_opened_can_be_replaced() {
        a_lost_speaker_can_be_replaced(true, true);
        a_lost_speaker_can_be_replaced(false, true);
    }

    #[test]
    fn the_game_gain_lives_in_an_atomic_apart_from_its_mute() {
        let control = AudioControl::default();
        assert_eq!(control.game_gain(), 1.0, "sin tocar, volumen normal");
        control.set_game_gain(0.5);
        assert_eq!(control.game_gain(), 0.5);
        control.set_game_gain(0.0);
        assert_eq!(
            control.game_gain(),
            0.0,
            "el cero no se confunde con el normal"
        );
        // Acotada a 0.0..=2.0; lo que no es un número cuenta como 1.0.
        control.set_game_gain(5.0);
        assert_eq!(control.game_gain(), MAX_VOICE_GAIN);
        control.set_game_gain(-1.0);
        assert_eq!(control.game_gain(), 0.0);
        control.set_game_gain(f32::NAN);
        assert_eq!(control.game_gain(), 1.0);
        control.set_game_gain(f32::INFINITY);
        assert_eq!(control.game_gain(), 1.0);
        // Silenciar no borra la ganancia guardada, ni la ganancia toca el silencio.
        control.set_game_gain(1.5);
        assert!(control.toggle_game());
        assert!(control.game_muted());
        assert_eq!(control.game_gain(), 1.5);
        control.set_game_muted(false);
        assert_eq!(control.game_gain(), 1.5);
        control.set_game_gain(0.0);
        assert!(!control.game_muted(), "a cero sigue sin estar silenciado");
        // Las voces no se enteran.
        assert_eq!(control.voice_gain(GAME_SOURCE), 1.0);
    }

    /// Mezcla una voz (un tono de `amplitude`) con la ganancia `gain` y devuelve el pico que suena.
    fn mixed_peak(gain: f32, amplitude: f64) -> i16 {
        let control = AudioControl::default();
        control.set_voice_gain(3, gain);
        mixed_peak_of(AudioTrack::Voice, &control, amplitude)
    }

    /// Mezcla el juego (un tono de `amplitude`) con `control` y devuelve el pico que suena.
    fn game_peak(control: &AudioControl, amplitude: f64) -> i16 {
        mixed_peak_of(AudioTrack::Game, control, amplitude)
    }

    /// Reproduce un solo emisor de `track` (un tono de `amplitude`, en todos sus canales) con los
    /// mandos de `control` y devuelve el pico que suena en la mezcla.
    fn mixed_peak_of(track: AudioTrack, control: &AudioControl, amplitude: f64) -> i16 {
        let profile = match track {
            AudioTrack::Game => Profile::Game,
            AudioTrack::Voice => Profile::Voice,
        };
        let channels = usize::from(profile.format().channels);
        let mut encoder = OpusEncoder::new(profile).expect("codificador");
        let mut stream = Stream::new(track, 3).expect("emisor");
        let pcm: Vec<i16> = (0..960)
            .flat_map(|n| {
                let sample = ((n as f64 * 0.04).sin() * amplitude) as i16;
                std::iter::repeat_n(sample, channels)
            })
            .collect();
        let mut packet = [0u8; coroom_audio::codec::MAX_PACKET];
        let base = Instant::now();
        for seq in 0..5u32 {
            let len = encoder.encode(&pcm, &mut packet).expect("codificar");
            stream.jitter.push_frames(
                seq,
                seq * 20_000,
                0,
                2,
                &packet[..len],
                base + Duration::from_millis(u64::from(seq) * 20),
            );
        }
        let (game, voices) = match track {
            AudioTrack::Game => (Some(stream), Vec::new()),
            AudioTrack::Voice => (None, vec![stream]),
        };
        let (_packets_tx, packets) = crossbeam_channel::bounded::<Packet>(1);
        let (links, _housekeeper) = links();
        let mut playout = Playout {
            control: control.clone(),
            render: None,
            packets,
            to_local: Box::new(|_| None),
            anchor: None,
            channels: 1,
            mixer: Mixer::new(1),
            game,
            voices,
            heard: Vec::with_capacity(MAX_VOICES + 1),
            av: None,
            ticks: 0,
            links,
            closed: Arc::new(AtomicBool::new(false)),
        };
        let mut out = [0i16; coroom_audio::SAMPLES_PER_CHANNEL];
        let mut peak = 0i16;
        for tick in 0..12u64 {
            assert!(playout.fill(&mut out, base + Duration::from_millis(30 + tick * 10)));
            peak = peak.max(out.iter().map(|s| s.saturating_abs()).max().unwrap_or(0));
        }
        peak
    }

    #[test]
    fn the_voice_gain_scales_the_mix_without_hard_clipping() {
        let normal = mixed_peak(1.0, 8_000.0);
        // Opus no es transparente: el pico decodificado se mueve algo alrededor del original.
        assert!((6_000..=10_000).contains(&normal), "{normal}");
        let double = mixed_peak(2.0, 8_000.0);
        let half = mixed_peak(0.5, 8_000.0);
        let ratio = f64::from(double) / f64::from(normal);
        assert!(
            (1.9..=2.1).contains(&ratio),
            "al doble: {normal} → {double}"
        );
        let ratio = f64::from(half) / f64::from(normal);
        assert!(
            (0.45..=0.55).contains(&ratio),
            "a la mitad: {normal} → {half}"
        );
        // Una voz fuerte al doble pasaría del fondo de escala: el limitador la baja justo hasta
        // el tope (sin aplastarla por debajo), sin recorte duro.
        let loud = mixed_peak(2.0, 20_000.0);
        assert!(loud > 30_000, "{loud}");
        assert_eq!(mixed_peak(0.0, 20_000.0), 0, "a cero no suena");
    }

    #[test]
    fn the_game_gain_scales_the_game_and_leaves_the_voices_alone() {
        let control = AudioControl::default();
        let normal = game_peak(&control, 8_000.0);
        assert!((6_000..=10_000).contains(&normal), "{normal}");
        control.set_game_gain(2.0);
        let double = game_peak(&control, 8_000.0);
        let ratio = f64::from(double) / f64::from(normal);
        assert!(
            (1.9..=2.1).contains(&ratio),
            "al doble: {normal} → {double}"
        );
        control.set_game_gain(0.5);
        let half = game_peak(&control, 8_000.0);
        let ratio = f64::from(half) / f64::from(normal);
        assert!(
            (0.45..=0.55).contains(&ratio),
            "a la mitad: {normal} → {half}"
        );
        control.set_game_gain(0.0);
        assert_eq!(game_peak(&control, 8_000.0), 0, "a cero no suena");
        // El silencio manda sobre la ganancia, y al quitarlo vuelve la ganancia guardada.
        control.set_game_gain(2.0);
        control.set_game_muted(true);
        assert_eq!(game_peak(&control, 8_000.0), 0, "silenciado no suena");
        control.set_game_muted(false);
        let again = game_peak(&control, 8_000.0);
        assert!(
            (f64::from(again) / f64::from(normal) - 2.0).abs() <= 0.1,
            "{again}"
        );
        // Una voz al volumen normal suena igual con el juego a la mitad o al doble.
        let voice = mixed_peak_of(AudioTrack::Voice, &AudioControl::default(), 8_000.0);
        control.set_game_gain(0.5);
        let with_quiet_game = mixed_peak_of(AudioTrack::Voice, &control, 8_000.0);
        control.set_game_gain(2.0);
        let with_loud_game = mixed_peak_of(AudioTrack::Voice, &control, 8_000.0);
        assert_eq!(
            (with_quiet_game, with_loud_game),
            (voice, voice),
            "las voces no cambian"
        );
    }

    #[test]
    fn the_flags_agree_with_the_jitter_buffer() {
        assert_eq!(AudioHeader::DTX, coroom_audio::jitter::DTX);
        assert_eq!(AudioHeader::FEC, coroom_audio::jitter::FEC);
        assert_eq!(AudioHeader::DRED, coroom_audio::jitter::DRED);
    }

    #[test]
    fn packets_parse_from_the_plaintext() {
        let header = AudioHeader {
            seq: 4,
            capture_us: 99,
            frames: 1,
            flags: 0,
        };
        let mut plain = header.to_bytes().to_vec();
        plain.extend_from_slice(&[1, 2, 3]);
        let packet = Packet::parse(AudioTrack::Voice, 7, &plain, Instant::now()).expect("paquete");
        assert_eq!((packet.header, packet.len, packet.source), (header, 3, 7));
        assert!(
            Packet::parse(
                AudioTrack::Voice,
                7,
                &plain[..AUDIO_HEADER_LEN],
                Instant::now()
            )
            .is_none()
        );
    }

    #[test]
    fn output_underruns_are_published_with_the_jitter_counters() {
        let ledger = LatencyLedger::new();
        let game = Stream::new(AudioTrack::Game, 1).expect("emisor");
        let device = DeviceStats::default();
        device.store(DeviceCounters {
            underruns: 7,
            overruns: 2,
        });
        publish(
            &ledger,
            std::iter::once((game.track, game.jitter.stats())),
            device.load(),
        );
        let summary = ledger.audio_summary().expect("resumen");
        let counters = summary
            .tracks
            .iter()
            .find(|track| track.track == AudioTrack::Game)
            .expect("juego")
            .counters;
        assert_eq!(
            (counters.device_underruns, counters.device_overruns),
            (7, 2)
        );
        assert!(
            summary.to_string().contains("salida sin audio 7"),
            "{summary}"
        );
    }

    #[test]
    fn streams_decode_twenty_millisecond_packets_in_two_ticks() {
        let mut encoder = OpusEncoder::new(Profile::Voice).expect("codificador");
        let mut stream = Stream::new(AudioTrack::Voice, 1).expect("emisor");
        let pcm: Vec<i16> = (0..960)
            .map(|n| ((n as f64 * 0.04).sin() * 8_000.0) as i16)
            .collect();
        let mut packet = [0u8; coroom_audio::codec::MAX_PACKET];
        let base = Instant::now();
        for seq in 0..5u32 {
            let len = encoder.encode(&pcm, &mut packet).expect("codificar");
            // Como el emisor real: la cabecera declara las dos tramas de 10 ms del paquete, que
            // es lo que le da al buffer el ritmo de las peticiones.
            stream.jitter.push_frames(
                seq,
                seq * 20_000,
                0,
                2,
                &packet[..len],
                base + Duration::from_millis(u64::from(seq) * 20),
            );
        }
        let mut sounded = 0;
        let mut seconds = 0;
        for tick in 0..12u64 {
            if let Some((offset, heard)) = stream.tick(base + Duration::from_millis(30 + tick * 10))
            {
                sounded += 1;
                if offset == 480 {
                    // La segunda trama de un paquete (o de la pérdida con que se cierra el
                    // flujo, sin marca de captura) nunca lleva llegada.
                    assert!(heard.arrived.is_none());
                    if heard.capture_us.is_some() {
                        seconds += 1;
                    }
                }
                assert!(offset <= 480);
            }
        }
        assert!(sounded >= 9, "{sounded}");
        assert!(
            seconds >= 4,
            "cada paquete de 20 ms da dos tramas: {seconds}"
        );
    }
}
