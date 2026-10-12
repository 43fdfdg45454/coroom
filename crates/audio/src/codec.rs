//! Opus (RFC 6716) con libopus **1.6.1** a través de la crate `opusic-c`.
//!
//! Elección de la crate (comprobada en crates.io en octubre de 2026):
//!
//! - `opusic-c` 1.6.1 / `opusic-sys` 0.7.5 (DoumanAsh, BSD-3-Clause, publicada en agosto de 2026):
//!   incluye libopus 1.6.1 con los pesos de DRED dentro del código fuente y la compila con CMake;
//!   los bindings están generados de antemano (sin `bindgen` ni libclang) y compila con MSVC sin
//!   dependencias del sistema. Expone FEC en banda, DTX, `OPUS_SET_DRED_DURATION` y el
//!   decodificador DRED (`opusic_c::dred`, feature `dred`).
//! - `opus-codec` 0.2.0 (libopus 1.5.2): su feature `dred` **descarga** el modelo al compilar con
//!   `sh`, `tar` y `wget`/`curl` (no funciona en Windows) y necesita `bindgen`. Se descarta.
//!
//! Perfiles:
//!
//! - [`Profile::Game`]: estéreo, `OPUS_APPLICATION_RESTRICTED_LOWDELAY` (solo CELT, 2,5 ms de
//!   *lookahead*), 128 kbit/s por defecto (128-160), VBR restringido. Sin FEC en banda (no existe
//!   en CELT): las pérdidas se ocultan con PLC.
//! - [`Profile::Voice`]: mono, `OPUS_APPLICATION_VOIP`, 32 kbit/s por defecto (32-48), DTX, FEC en
//!   banda (LBRR de SILK, con una pérdida esperada del 10 %) y, en Linux, **DRED** con 100 ms de
//!   redundancia. En Windows, FEC clásica (`dred_available()` es `false`).
//!
//! Sin asignaciones en caliente: [`OpusEncoder::encode`] escribe en el búfer del llamante y
//! [`OpusDecoder::decode`] en la trama de salida del llamante.

use opusic_c::{
    Application, Bitrate, Channels, Decoder, Encoder, ErrorCode, InbandFec, SampleRate, Signal,
};

use crate::{AudioError, AudioFormat};

/// Bytes máximos de un paquete Opus de una trama (RFC 6716 §3.4: 1275).
pub const MAX_PACKET: usize = 1275;

/// Tramas de 10 ms de redundancia DRED que pide el perfil de voz (100 ms).
pub const DRED_FRAMES: u8 = 10;

/// Pérdida que se le dice al codificador de voz para dimensionar la FEC en banda, en %.
pub const VOICE_EXPECTED_LOSS: u8 = 10;

fn codec_error(what: &str) -> impl Fn(ErrorCode) -> AudioError + '_ {
    move |code| AudioError::Codec(format!("{what}: {code:?}"))
}

/// Versión de libopus enlazada.
pub fn libopus_version() -> &'static str {
    let full = opusic_c::version();
    full.strip_prefix("libopus ").unwrap_or(full)
}

/// Si este binario decodifica DRED (Linux; en Windows se usa la FEC clásica).
pub const fn dred_available() -> bool {
    cfg!(target_os = "linux")
}

/// Perfil del codificador.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Audio del juego: estéreo, `restricted low-delay`.
    Game,
    /// Voz: mono, `voip`, DTX, FEC y DRED.
    Voice,
}

impl Profile {
    /// Formato PCM del perfil.
    pub const fn format(self) -> AudioFormat {
        match self {
            Profile::Game => AudioFormat::GAME,
            Profile::Voice => AudioFormat::VOICE,
        }
    }

    /// Bitrate por defecto en bit/s.
    pub const fn default_bitrate(self) -> u32 {
        match self {
            Profile::Game => 128_000,
            Profile::Voice => 32_000,
        }
    }

    /// Bitrates admitidos en bit/s (128-160 y 32-48 kbit/s).
    pub const fn bitrate_range(self) -> (u32, u32) {
        match self {
            Profile::Game => (128_000, 160_000),
            Profile::Voice => (32_000, 48_000),
        }
    }

    const fn channels(self) -> Channels {
        match self {
            Profile::Game => Channels::Stereo,
            Profile::Voice => Channels::Mono,
        }
    }
}

/// Cómo se reconstruyó una trama perdida.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Recovery {
    /// Con la FEC en banda (LBRR) del paquete siguiente.
    Fec,
    /// Con la redundancia profunda (DRED) del paquete siguiente.
    Dred,
    /// Ocultación (PLC) sin datos.
    Plc,
}

/// Muestras por canal de un paquete Opus a 48 kHz (480 para 10 ms, 960 para 20 ms).
pub fn packet_samples(packet: &[u8]) -> Option<usize> {
    opusic_c::utils::get_nb_samples(packet, SampleRate::Hz48000).ok()
}

/// Indica si un paquete es de SILK o híbrido (los únicos modos con FEC en banda), por su TOC.
pub fn is_silk_or_hybrid(packet: &[u8]) -> bool {
    packet.first().is_some_and(|toc| (toc >> 3) < 16)
}

/// Indica si el codificador emitió una trama de DTX (1 o 2 bytes: no hace falta enviarla).
pub const fn is_dtx_packet(len: usize) -> bool {
    len <= 2
}

/// Codificador Opus de un perfil.
pub struct OpusEncoder {
    inner: Encoder,
    profile: Profile,
    frame_samples: usize,
    bitrate: u32,
    dred: bool,
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("profile", &self.profile)
            .field("bitrate", &self.bitrate)
            .field("dred", &self.dred)
            .finish_non_exhaustive()
    }
}

impl OpusEncoder {
    /// Codificador de `profile` con su bitrate por defecto.
    pub fn new(profile: Profile) -> Result<Self, AudioError> {
        let application = match profile {
            Profile::Game => Application::LowDelay,
            Profile::Voice => Application::Voip,
        };
        let mut inner = Encoder::new(profile.channels(), SampleRate::Hz48000, application)
            .map_err(codec_error("no se pudo crear el codificador"))?;
        let setup = codec_error("no se pudo configurar el codificador");
        inner.set_complexity(9).map_err(&setup)?;
        inner.set_vbr(true).map_err(&setup)?;
        inner.set_vbr_constraint(true).map_err(&setup)?;
        let mut dred = false;
        match profile {
            Profile::Game => {
                inner.set_signal(Signal::Music).map_err(&setup)?;
            }
            Profile::Voice => {
                inner.set_signal(Signal::Voice).map_err(&setup)?;
                inner.set_dtx(true).map_err(&setup)?;
                inner.set_inband_fec(InbandFec::Mode1).map_err(&setup)?;
                inner.set_packet_loss(VOICE_EXPECTED_LOSS).map_err(&setup)?;
                if dred_available() {
                    dred = inner.set_dred_duration(DRED_FRAMES).is_ok();
                }
            }
        }
        let mut encoder = Self {
            inner,
            profile,
            frame_samples: profile.format().samples_per_frame(),
            bitrate: 0,
            dred,
        };
        encoder.set_bitrate(profile.default_bitrate())?;
        Ok(encoder)
    }

    /// Perfil.
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// Fija el bitrate en bit/s (se ajusta al intervalo del perfil). Devuelve el aplicado.
    pub fn set_bitrate(&mut self, bits_per_second: u32) -> Result<u32, AudioError> {
        let (min, max) = self.profile.bitrate_range();
        let bitrate = bits_per_second.clamp(min, max);
        self.inner
            .set_bitrate(Bitrate::Value(bitrate))
            .map_err(codec_error("no se pudo fijar el bitrate"))?;
        self.bitrate = bitrate;
        Ok(bitrate)
    }

    /// Bitrate actual en bit/s.
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Pérdida esperada en % (la FEC y DRED se dimensionan con ella).
    pub fn set_expected_loss(&mut self, percent: u8) -> Result<(), AudioError> {
        self.inner
            .set_packet_loss(percent.min(100))
            .map_err(codec_error("no se pudo fijar la pérdida esperada"))
    }

    /// Si el perfil lleva FEC en banda.
    pub fn fec(&self) -> bool {
        self.profile == Profile::Voice
    }

    /// Si el codificador incrusta DRED.
    pub fn dred(&self) -> bool {
        self.dred
    }

    /// Muestras por canal que el codificador adelanta (su retardo algorítmico).
    pub fn lookahead(&mut self) -> Result<usize, AudioError> {
        self.inner
            .get_look_ahead()
            .map(|samples| samples as usize)
            .map_err(codec_error("no se pudo leer el lookahead"))
    }

    /// Codifica 10 o 20 ms (`pcm`, intercalada) en `out`. Devuelve los bytes del paquete
    /// (1 o 2 en una trama de DTX, que no hace falta enviar).
    pub fn encode(&mut self, pcm: &[i16], out: &mut [u8]) -> Result<usize, AudioError> {
        if pcm.len() != self.frame_samples && pcm.len() != 2 * self.frame_samples {
            return Err(AudioError::BufferTooSmall {
                needed: self.frame_samples,
                available: pcm.len(),
            });
        }
        let input: &[u16] = bytemuck::cast_slice(pcm);
        self.inner
            .encode_to_slice(input, out)
            .map_err(codec_error("no se pudo codificar la trama"))
    }
}

enum Inner {
    Plain(Decoder),
    #[cfg(target_os = "linux")]
    Dred(Box<opusic_c::dred::Dred>),
}

/// Decodificador Opus de un perfil (uno por emisor).
pub struct OpusDecoder {
    inner: Inner,
    frame_samples: usize,
}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder")
            .field("frame_samples", &self.frame_samples)
            .finish_non_exhaustive()
    }
}

impl OpusDecoder {
    /// Decodificador de `profile` (con DRED en Linux).
    pub fn new(profile: Profile) -> Result<Self, AudioError> {
        let decoder = Decoder::new(profile.channels(), SampleRate::Hz48000)
            .map_err(codec_error("no se pudo crear el decodificador"))?;
        #[cfg(target_os = "linux")]
        let inner = match profile {
            Profile::Voice => match opusic_c::dred::Dred::new(decoder) {
                Ok(dred) => Inner::Dred(Box::new(dred)),
                Err(err) => return Err(codec_error("no se pudo crear el decodificador DRED")(err)),
            },
            Profile::Game => Inner::Plain(decoder),
        };
        #[cfg(not(target_os = "linux"))]
        let inner = Inner::Plain(decoder);
        Ok(Self {
            inner,
            frame_samples: profile.format().samples_per_frame(),
        })
    }

    fn decoder(&mut self) -> &mut Decoder {
        match &mut self.inner {
            Inner::Plain(decoder) => decoder,
            #[cfg(target_os = "linux")]
            Inner::Dred(dred) => dred.decoder_mut(),
        }
    }

    fn frame<'a>(&self, out: &'a mut [i16]) -> Result<&'a mut [u16], AudioError> {
        self.frames(1, out)
    }

    fn frames<'a>(&self, frames: usize, out: &'a mut [i16]) -> Result<&'a mut [u16], AudioError> {
        let available = out.len();
        let needed = self.frame_samples * frames.clamp(1, 2);
        let frame = out
            .get_mut(..needed)
            .ok_or(AudioError::BufferTooSmall { needed, available })?;
        Ok(bytemuck::cast_slice_mut(frame))
    }

    /// Decodifica un paquete de 10 o 20 ms (`frames` = 1 o 2) en `out`; con `None`, oculta
    /// `frames` tramas. Devuelve las muestras intercaladas escritas.
    pub fn decode_frames(
        &mut self,
        packet: Option<&[u8]>,
        frames: usize,
        out: &mut [i16],
    ) -> Result<usize, AudioError> {
        let channels = self.frame_samples / crate::SAMPLES_PER_CHANNEL;
        let frame = self.frames(frames, out)?;
        let decoded = self
            .decoder()
            .decode_to_slice(packet.unwrap_or(&[]), frame, false)
            .map_err(codec_error("no se pudo decodificar la trama"))?;
        Ok(decoded * channels)
    }

    /// Decodifica `packet` en `out` (una trama de 10 ms); con `None`, ocultación (PLC). Devuelve
    /// las muestras intercaladas escritas.
    pub fn decode(&mut self, packet: Option<&[u8]>, out: &mut [i16]) -> Result<usize, AudioError> {
        self.decode_frames(packet, 1, out)
    }

    /// Reconstruye la trama perdida justo antes de `next` (el paquete siguiente, si llegó): con
    /// su FEC en banda si `next_fec`, con su DRED si `next_dred` y este binario lo decodifica, o
    /// con PLC. Escribe una trama en `out`.
    pub fn recover(
        &mut self,
        next: Option<&[u8]>,
        next_fec: bool,
        next_dred: bool,
        out: &mut [i16],
    ) -> Result<Recovery, AudioError> {
        if let Some(next) = next.filter(|packet| !packet.is_empty()) {
            if next_fec && is_silk_or_hybrid(next) {
                let frame = self.frame(out)?;
                if self
                    .decoder()
                    .decode_to_slice(next, frame, true)
                    .is_ok_and(|samples| samples > 0)
                {
                    return Ok(Recovery::Fec);
                }
            }
            #[cfg(target_os = "linux")]
            if next_dred {
                let frame = self.frame(out)?;
                if let Inner::Dred(dred) = &mut self.inner
                    && dred
                        .decode_to_slice(next, frame)
                        .is_ok_and(|samples| samples > 0)
                {
                    return Ok(Recovery::Dred);
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = next_dred;
        self.decode(None, out)?;
        Ok(Recovery::Plc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SAMPLES_PER_CHANNEL;

    /// Tono de `freq` Hz a `amplitude` (0-1) en `channels` canales, desde la muestra `start`.
    fn tone(freq: f64, amplitude: f64, channels: usize, start: usize, len: usize) -> Vec<i16> {
        let mut out = Vec::with_capacity(len * channels);
        for n in start..start + len {
            let v = (2.0 * std::f64::consts::PI * freq * n as f64 / 48_000.0).sin() * amplitude;
            for _ in 0..channels {
                out.push((v * 32_767.0) as i16);
            }
        }
        out
    }

    fn encode_decode(profile: Profile, input: &[i16]) -> Vec<i16> {
        let mut encoder = OpusEncoder::new(profile).expect("codificador");
        let mut decoder = OpusDecoder::new(profile).expect("decodificador");
        let frame = profile.format().samples_per_frame();
        let mut packet = [0u8; MAX_PACKET];
        let mut out = vec![0i16; frame];
        let mut decoded = Vec::with_capacity(input.len());
        for chunk in input.chunks_exact(frame) {
            let len = encoder.encode(chunk, &mut packet).expect("codificar");
            let n = decoder
                .decode(Some(&packet[..len]), &mut out)
                .expect("decodificar");
            assert_eq!(n, frame);
            decoded.extend_from_slice(&out);
        }
        decoded
    }

    /// Retardo (en muestras por canal) que maximiza la correlación del primer canal.
    fn best_delay(input: &[i16], output: &[i16], channels: usize, max: usize) -> usize {
        let a: Vec<f64> = input
            .iter()
            .step_by(channels)
            .map(|s| f64::from(*s))
            .collect();
        let b: Vec<f64> = output
            .iter()
            .step_by(channels)
            .map(|s| f64::from(*s))
            .collect();
        let window = 9_600.min(a.len().saturating_sub(max));
        let base = 4_800;
        (0..max)
            .max_by(|x, y| {
                let corr = |d: usize| -> f64 {
                    (base..base + window).map(|i| a[i] * b[i + d]).sum::<f64>()
                };
                corr(*x).total_cmp(&corr(*y))
            })
            .unwrap_or(0)
    }

    fn snr_db(input: &[i16], output: &[i16], channels: usize, delay: usize) -> f64 {
        let skip = 4_800 * channels;
        let mut signal = 0.0;
        let mut noise = 0.0;
        for i in skip..input.len() - delay * channels {
            let x = f64::from(input[i]);
            let y = f64::from(output[i + delay * channels]);
            signal += x * x;
            noise += (x - y) * (x - y);
        }
        10.0 * (signal / noise.max(1e-9)).log10()
    }

    /// Ruido pseudoaleatorio (sin periodo: el retardo medido no es ambiguo como con un tono).
    fn noise(channels: usize, len: usize) -> Vec<i16> {
        let mut seed = 12_345u64;
        let mut out = Vec::with_capacity(len * channels);
        for _ in 0..len {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let v = ((seed >> 49) as i64 - 16_384) as i16;
            for _ in 0..channels {
                out.push(v);
            }
        }
        out
    }

    #[test]
    fn game_profile_round_trips_a_tone_with_low_delay() {
        // Retardo algorítmico medido de verdad, con ruido.
        let probe = noise(2, 48_000);
        let delay = best_delay(&probe, &encode_decode(Profile::Game, &probe), 2, 960);
        let delay_ms = delay as f64 / 48.0;
        let input = tone(440.0, 0.5, 2, 0, 48_000);
        let output = encode_decode(Profile::Game, &input);
        let snr = snr_db(&input, &output, 2, delay);
        println!("juego: retardo algorítmico medido {delay_ms:.2} ms, SNR {snr:.1} dB");
        assert!(delay_ms < 7.0, "retardo {delay_ms:.2} ms");
        assert!(snr > 25.0, "SNR {snr:.1} dB");
        let mut encoder = OpusEncoder::new(Profile::Game).expect("codificador");
        assert_eq!(encoder.lookahead().expect("lookahead"), delay);
        assert!(!encoder.fec());
    }

    #[test]
    fn voice_profile_round_trips_a_tone() {
        let probe = noise(1, 48_000);
        let delay = best_delay(&probe, &encode_decode(Profile::Voice, &probe), 1, 960);
        let input = tone(300.0, 0.4, 1, 0, 48_000);
        let output = encode_decode(Profile::Voice, &input);
        let snr = snr_db(&input, &output, 1, delay);
        println!(
            "voz: retardo {:.2} ms, SNR {snr:.1} dB",
            delay as f64 / 48.0
        );
        // SILK es paramétrico: no copia la forma de onda de un tono puro (unos 8 dB), pero lo
        // reproduce; el umbral de 25 dB es el del audio del juego.
        assert!(snr > 5.0, "SNR {snr:.1} dB");
        assert!(delay < 7 * 48, "retardo de la voz {delay}");
    }

    #[test]
    fn bitrates_stay_in_the_profile_range() {
        let mut game = OpusEncoder::new(Profile::Game).expect("codificador");
        assert_eq!(game.bitrate(), 128_000);
        assert_eq!(game.set_bitrate(500_000), Ok(160_000));
        let mut voice = OpusEncoder::new(Profile::Voice).expect("codificador");
        assert_eq!(voice.set_bitrate(8_000), Ok(32_000));
        assert!(voice.fec());
        assert_eq!(voice.dred(), dred_available());
        assert!(!libopus_version().is_empty());
        assert!(libopus_version().starts_with("1."), "{}", libopus_version());
    }

    #[test]
    fn plc_fec_and_dred_fill_a_lost_frame() {
        let mut encoder = OpusEncoder::new(Profile::Voice).expect("codificador");
        let mut decoder = OpusDecoder::new(Profile::Voice).expect("decodificador");
        let mut packets = Vec::new();
        let mut packet = [0u8; MAX_PACKET];
        // Un tono modulado (como sílabas) para que SILK lo trate como voz activa.
        for frame in 0..60 {
            let mut pcm = tone(
                220.0,
                0.4,
                1,
                frame * SAMPLES_PER_CHANNEL,
                SAMPLES_PER_CHANNEL,
            );
            let envelope = 0.6 + 0.4 * ((frame as f64) * 0.7).sin();
            for s in &mut pcm {
                *s = (f64::from(*s) * envelope) as i16;
            }
            let len = encoder.encode(&pcm, &mut packet).expect("codificar");
            packets.push(packet[..len].to_vec());
        }
        let mut out = [0i16; SAMPLES_PER_CHANNEL];
        let mut seen = std::collections::HashSet::new();
        for (index, packet) in packets.iter().enumerate() {
            if index > 20 && index % 7 == 0 {
                let next = packets.get(index + 1).map(Vec::as_slice);
                let how = decoder
                    .recover(next, index % 2 == 0, index % 2 == 1, &mut out)
                    .expect("recuperar");
                seen.insert(how);
                assert!(out.iter().any(|s| *s != 0), "{how:?} rellena la trama");
            } else {
                decoder.decode(Some(packet), &mut out).expect("decodificar");
            }
        }
        decoder.recover(None, true, true, &mut out).expect("PLC");
        println!("recuperaciones: {seen:?}");
        assert!(seen.contains(&Recovery::Fec), "{seen:?}");
        if dred_available() {
            assert!(seen.contains(&Recovery::Dred), "{seen:?}");
        }
        assert_eq!(
            decoder.recover(None, false, false, &mut out),
            Ok(Recovery::Plc)
        );
    }

    #[test]
    fn twenty_millisecond_packets_round_trip() {
        let mut encoder = OpusEncoder::new(Profile::Voice).expect("codificador");
        let mut decoder = OpusDecoder::new(Profile::Voice).expect("decodificador");
        let pcm = tone(300.0, 0.3, 1, 0, 2 * SAMPLES_PER_CHANNEL);
        let mut packet = [0u8; MAX_PACKET];
        let len = encoder.encode(&pcm, &mut packet).expect("20 ms");
        assert_eq!(packet_samples(&packet[..len]), Some(960));
        let mut out = [0i16; 960];
        assert_eq!(
            decoder.decode_frames(Some(&packet[..len]), 2, &mut out),
            Ok(960)
        );
        assert_eq!(decoder.decode_frames(None, 1, &mut out), Ok(480));
    }

    #[test]
    fn short_buffers_are_rejected() {
        let mut encoder = OpusEncoder::new(Profile::Game).expect("codificador");
        let mut packet = [0u8; MAX_PACKET];
        assert!(matches!(
            encoder.encode(&[0; 10], &mut packet),
            Err(AudioError::BufferTooSmall { needed: 960, .. })
        ));
        let mut decoder = OpusDecoder::new(Profile::Game).expect("decodificador");
        let mut out = [0i16; 10];
        assert!(decoder.decode(None, &mut out).is_err());
        assert!(is_dtx_packet(1) && !is_dtx_packet(3));
        assert!(!is_silk_or_hybrid(&[]));
    }
}
