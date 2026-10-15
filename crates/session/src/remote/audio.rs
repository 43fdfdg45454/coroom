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
//! fotograma de captura más cercana y ajusta el buffer del juego. Cada trama deja sus marcas en el
//! [`LatencyLedger`] ([`AudioStage`]) y los contadores se publican cada medio segundo.
//!
//! Sin asignaciones en caliente: los búferes PCM, Opus, SFrame y de paquetes se reservan al
//! arrancar (los paquetes viajan por el canal por valor, con su carga en un array). La única
//! asignación por trama es el `Bytes` del datagrama que se entrega a `quinn`, que se queda con él
//! hasta enviarlo (y que se comparte entre todos los destinos).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use coroom_audio::codec::{OpusDecoder, OpusEncoder, Profile, Recovery, is_silk_or_hybrid};
use coroom_audio::jitter::{JitterBuffer, Pop};
use coroom_audio::mix::Mixer;
use coroom_audio::processing::{ProcessingOptions, RenderFeed, VoiceProcessor};
use coroom_audio::sync::AvSync;
use coroom_audio::{
    AudioConfig, AudioSink, AudioSource, DeviceCounters, MAX_FRAME_SAMPLES, Render,
};
use coroom_transport::PeerConnection;
use coroom_transport::crypto::sframe::FrameEncryptor;
use coroom_transport::proto::audio::{AUDIO_HEADER_LEN, AudioHeader};
use coroom_transport::proto::media::Track;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use tracing::debug;

use super::{Core, lock};
use crate::latency::{
    AudioCounters, AudioSide, AudioStage, AudioTimeline, AudioTrack, AvSummary, LatencyLedger,
};

/// Bytes máximos de un paquete Opus en el cable.
pub const MAX_PACKET: usize = coroom_audio::jitter::MAX_PACKET;

/// Paquetes en vuelo entre la red y la reproducción.
const PACKET_QUEUE: usize = 512;

/// Cada cuántas tramas de reproducción se publican contadores y se reajusta la sincronía.
const PUBLISH_TICKS: u32 = 50;

/// Un emisor que no envía en este tiempo se olvida (su buffer y su decodificador).
const IDLE_SOURCE: Duration = Duration::from_secs(60);

/// Cada cuánto mira `audio-play` si el dispositivo a demanda sigue en marcha.
const PULL_WATCH: Duration = Duration::from_millis(20);

/// Emisores de voz como mucho a la vez.
const MAX_VOICES: usize = 16;

/// Fuente del juego en el mezclador.
const GAME_SOURCE: u32 = u32::MAX;

/// Bitrate por defecto del juego (bit/s).
pub const DEFAULT_GAME_BITRATE: u32 = 128_000;

/// Bitrate por defecto de la voz (bit/s).
pub const DEFAULT_VOICE_BITRATE: u32 = 32_000;

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
    /// invitado en el anfitrión). Solo afecta a lo que se reproduce aquí.
    muted_voices: Mutex<BTreeSet<u32>>,
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
        let mut set = lock(&self.inner.muted_voices);
        if muted {
            set.insert(source);
        } else {
            set.remove(&source);
        }
    }

    /// Si la voz de `source` está silenciada aquí.
    pub fn voice_muted(&self, source: u32) -> bool {
        lock(&self.inner.muted_voices).contains(&source)
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

/// Hilo `voice-cap`: micrófono → procesado → VAD o pulsar para hablar → Opus → SFrame → destinos.
pub(crate) fn voice_capture_loop(
    core: &Core,
    mut input: VoiceInput,
    mut processor: VoiceProcessor,
    frame_ms: u8,
    control: &AudioControl,
    out: &dyn AudioOut,
) {
    realtime("voice-cap");
    let shared = &core.shared;
    if let Err(err) = input.mic.start(&AudioConfig::voice()) {
        core.warn(format!("sin micrófono: {err}"));
        control.set_voice_enabled(false);
        return;
    }
    let mut sealer = match Sealer::new(core, Profile::Voice, Track::Voice, input.bitrate) {
        Ok(sealer) => sealer,
        Err(err) => {
            core.warn(format!("sin voz: {err}"));
            return;
        }
    };
    debug!(micrófono = %input.mic.describe(), procesado = processor.backend(), "voz");
    let frames = frames_of(frame_ms);
    let mut frame = Vec::with_capacity(MAX_FRAME_SAMPLES);
    let mut pcm = Vec::with_capacity(MAX_FRAME_SAMPLES * 2);
    let mut talking = false;
    let mut leaf = core.own_leaf.load(Ordering::Acquire);
    while !shared.stopping() {
        let captured_at = match read_packet(input.mic.as_mut(), frames, &mut frame, &mut pcm) {
            Ok(at) => at,
            Err(err) => {
                core.warn(format!("el micrófono se detuvo: {err}"));
                break;
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
        timeline.mark(AudioStage::Processed, Instant::now());
        let wants = if control.push_to_talk() {
            control.inner.talk_pressed.load(Ordering::Acquire)
        } else {
            voice
        };
        let now_talking = wants && !control.mic_muted() && control.voice_allowed();
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

/// Guarda un paquete en el buffer de su emisor (lo crea si es nuevo).
fn accept(core: &Core, packet: Packet, game: &mut Option<Stream>, voices: &mut Vec<Stream>) {
    let stream = match packet.track {
        AudioTrack::Game => {
            if game.is_none() {
                match Stream::new(AudioTrack::Game, packet.source) {
                    Ok(stream) => *game = Some(stream),
                    Err(err) => {
                        core.warn(format!("audio del juego: {err}"));
                        return;
                    }
                }
            }
            game.as_mut()
        }
        AudioTrack::Voice => {
            if !voices.iter().any(|v| v.source == packet.source) {
                if voices.len() >= MAX_VOICES {
                    return;
                }
                match Stream::new(AudioTrack::Voice, packet.source) {
                    Ok(stream) => voices.push(stream),
                    Err(err) => {
                        core.warn(format!("voz: {err}"));
                        return;
                    }
                }
            }
            voices.iter_mut().find(|v| v.source == packet.source)
        }
    };
    if let Some(stream) = stream {
        stream.last_packet = packet.arrived;
        stream.jitter.push(
            packet.header.seq,
            packet.header.capture_us,
            packet.header.flags,
            &packet.data[..usize::from(packet.len)],
            packet.arrived,
        );
    }
}

/// La reproducción trama a trama: buffers, decodificación, mezcla, marcas y contadores. La mueve
/// el hilo `audio-play` con la cola del sumidero o, a demanda, el propio dispositivo.
struct Playout {
    core: Arc<Core>,
    control: AudioControl,
    render: Option<RenderFeed>,
    packets: Receiver<Packet>,
    to_local: Box<ToLocal>,
    channels: u16,
    mixer: Mixer,
    game: Option<Stream>,
    voices: Vec<Stream>,
    heard: Vec<Heard>,
    av: Option<AvSync>,
    last_video: u64,
    ticks: u32,
    /// Se cerró el canal de paquetes: la sesión termina.
    closed: Arc<AtomicBool>,
    /// Faltas y sobras de la salida.
    device: Arc<DeviceStats>,
}

impl Playout {
    /// Llena `out` (una trama) con lo que toca en `now`, con lo más reciente que haya llegado.
    /// `false` si el canal de paquetes se cerró.
    fn fill(&mut self, out: &mut [i16], now: Instant) -> bool {
        loop {
            match self.packets.try_recv() {
                Ok(packet) => accept(&self.core, packet, &mut self.game, &mut self.voices),
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

    /// `out`, que [`Playout::fill`] llenó en `decided`, sonará en `played_at`: marcas, sincronía
    /// y, cada [`PUBLISH_TICKS`] tramas, contadores.
    fn played(&mut self, out: &[i16], decided: Instant, played_at: Instant) {
        let ledger = &self.core.ledger;
        if let Some(render) = &mut self.render {
            render.feed_render(out, self.channels);
        }

        // Marcas de lo que sonó (las tramas reales; lo ocultado no tiene captura).
        for info in &self.heard {
            let Some(capture) = info.capture_us.and_then(&self.to_local) else {
                continue;
            };
            let mut timeline = AudioTimeline::default();
            timeline.mark(AudioStage::Captured, capture);
            if let Some(arrived) = info.arrived {
                timeline.mark(AudioStage::Received, arrived);
            }
            timeline.mark(AudioStage::Decoded, decided);
            timeline.mark(AudioStage::Played, played_at);
            ledger.record_audio(info.track, AudioSide::Receiver, timeline);
            if info.track == AudioTrack::Game
                && let Some(av) = &mut self.av
            {
                av.on_audio(capture, played_at);
            }
        }

        self.ticks = self.ticks.wrapping_add(1);
        if let Some(av) = &mut self.av
            && self.ticks.is_multiple_of(10)
        {
            ledger.video_pairs_since(&mut self.last_video, |captured, presented| {
                av.on_video(captured, presented);
            });
        }
        if self.ticks.is_multiple_of(PUBLISH_TICKS) {
            if let (Some(av), Some(stream)) = (&mut self.av, &mut self.game) {
                let (min, max) = stream.jitter.extra_delay_range_ms();
                av.set_limits(min, max);
                let adjust = av.recommend();
                stream.jitter.set_extra_delay(adjust);
                let stats = av.stats();
                ledger.set_av(AvSummary {
                    samples: stats.samples,
                    p50_ms: stats.p50_abs_ms,
                    p95_ms: stats.p95_abs_ms,
                    offset_ms: stats.offset_ms,
                    adjust_ms: stats.adjust_ms,
                });
            }
            self.publish();
            self.voices.retain(|stream| {
                decided.saturating_duration_since(stream.last_packet) < IDLE_SOURCE
            });
            let speaking = self
                .control
                .talkers()
                .into_iter()
                .map(talker_label)
                .collect();
            ledger.set_voice_status(speaking, self.control.status_label());
        }
    }

    fn publish(&self) {
        publish(
            &self.core.ledger,
            self.game.iter().chain(self.voices.iter()),
            self.device.load(),
        );
    }
}

/// Hilo `audio-play`. Con un sumidero a demanda ([`AudioSink::start_pull`]) el dispositivo pide
/// cada trama justo cuando la necesita y este hilo solo vigila; si no, la decide este hilo en
/// cuanto el sumidero tiene sitio, una trama antes de que suene.
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
    let closed = Arc::new(AtomicBool::new(false));
    let device = Arc::new(DeviceStats::default());
    let playout = Playout {
        core: Arc::clone(core),
        control,
        render,
        packets,
        to_local: Box::new(to_local),
        channels: format.channels,
        mixer: Mixer::new(format.channels),
        game: None,
        voices: Vec::with_capacity(MAX_VOICES),
        heard: Vec::with_capacity(MAX_VOICES + 1),
        av: av_sync.then(AvSync::new),
        last_video: 0,
        ticks: 0,
        closed: Arc::clone(&closed),
        device: Arc::clone(&device),
    };
    let frame_len = coroom_audio::SAMPLES_PER_CHANNEL * usize::from(format.channels);
    if sink.supports_pull() {
        play_pull(core, sink.as_mut(), &config, playout, &closed, &device);
    } else {
        play_push(core, sink.as_mut(), &config, playout, frame_len, &device);
    }
}

/// A demanda: el dispositivo llama a la reproducción desde su hilo.
fn play_pull(
    core: &Core,
    sink: &mut dyn AudioSink,
    config: &AudioConfig,
    playout: Playout,
    closed: &AtomicBool,
    device_stats: &DeviceStats,
) {
    let playout = Arc::new(Mutex::new(playout));
    let device = Arc::clone(&playout);
    let render: Render = Box::new(move |out, played_at| {
        let mut playout = lock(&device);
        let decided = Instant::now();
        if playout.fill(out, decided) {
            playout.played(out, decided, played_at);
        } else {
            out.fill(0);
        }
    });
    if let Err(err) = sink.start_pull(config, render) {
        core.warn(format!("sin salida de audio: {err}"));
        return;
    }
    debug!(salida = %sink.describe(), "reproducción de audio a demanda");
    while !core.shared.stopping() && !closed.load(Ordering::Acquire) {
        if let Err(err) = sink.check() {
            core.warn(format!("la salida de audio se detuvo: {err}"));
            break;
        }
        device_stats.store(sink.device_counters());
        std::thread::sleep(PULL_WATCH);
    }
    device_stats.store(sink.device_counters());
    sink.stop();
    lock(&playout).publish();
}

/// Con la cola del sumidero: se espera su sitio y después se decide qué suena.
fn play_push(
    core: &Core,
    sink: &mut dyn AudioSink,
    config: &AudioConfig,
    mut playout: Playout,
    frame_len: usize,
    device: &DeviceStats,
) {
    // Con la cola, este hilo marca el ritmo del sumidero (a demanda lo marca el dispositivo).
    realtime("audio-play");
    if let Err(err) = sink.start(config) {
        core.warn(format!("sin salida de audio: {err}"));
        return;
    }
    debug!(salida = %sink.describe(), "reproducción de audio");
    let mut out = [0i16; MAX_FRAME_SAMPLES];
    while !core.shared.stopping() {
        if let Err(err) = sink.wait_ready() {
            core.warn(format!("la salida de audio se detuvo: {err}"));
            break;
        }
        let decided = Instant::now();
        if !playout.fill(&mut out[..frame_len], decided) {
            return;
        }
        let played = match sink.write(&out[..frame_len]) {
            Ok(played) => played,
            Err(err) => {
                core.warn(format!("la salida de audio se detuvo: {err}"));
                break;
            }
        };
        device.store(sink.device_counters());
        playout.played(&out[..frame_len], decided, played.played_at_estimate);
    }
    device.store(sink.device_counters());
    playout.publish();
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
fn publish<'a>(
    ledger: &LatencyLedger,
    streams: impl Iterator<Item = &'a Stream>,
    device: DeviceCounters,
) {
    let mut totals = [AudioCounters {
        device_underruns: device.underruns,
        device_overruns: device.overruns,
        ..AudioCounters::default()
    }; 2];
    let mut seen = [false; 2];
    for stream in streams {
        let index = match stream.track {
            AudioTrack::Game => 0,
            AudioTrack::Voice => 1,
        };
        seen[index] = true;
        let stats = stream.jitter.stats();
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
        publish(&ledger, std::iter::once(&game), device.load());
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
            stream.jitter.push(
                seq,
                seq * 20_000,
                0,
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
                    seconds += 1;
                    assert!(heard.arrived.is_none() && heard.capture_us.is_some());
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
