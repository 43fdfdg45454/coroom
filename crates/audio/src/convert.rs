//! Conversión entre las tramas del pipeline (PCM `i16` intercalado a 48 kHz, uno o dos canales) y
//! el formato de un dispositivo que no convierte por nosotros: el formato de mezcla del motor de
//! WASAPI en el modo compartido de baja latencia (`IAudioClient3`), que es coma flotante de 32 bits
//! con los canales del dispositivo, o enteros de 16 o 32 bits. Solo a 48 kHz: la frecuencia no se
//! convierte aquí (si el motor mezcla a otra, el backend abre el flujo en el modo clásico con la
//! conversión automática del motor).
//!
//! Canales: con los mismos, se copian. Nuestro mono va a los dos primeros canales del
//! dispositivo (los frontales) y los demás quedan en silencio; nuestro estéreo, a los dos
//! primeros. Al capturar, un dispositivo con más canales que los nuestros se promedia a mono, o
//! se toman sus dos primeros canales para estéreo.

/// Tipo de muestra del dispositivo, en little-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sample {
    /// Entero de 16 bits con signo.
    Int16,
    /// Entero de 32 bits con signo con `valid_bits` bits útiles alineados a la izquierda (24 en
    /// 32, o 32).
    Int32 {
        /// Bits con información (1-32).
        valid_bits: u16,
    },
    /// Coma flotante de 32 bits en `[-1, 1]`.
    Float32,
}

impl Sample {
    /// Bytes de una muestra.
    pub(crate) const fn bytes(self) -> usize {
        match self {
            Sample::Int16 => 2,
            Sample::Int32 { .. } | Sample::Float32 => 4,
        }
    }

    /// Escribe `value` en `out` (los bytes de una muestra).
    fn write(self, value: i16, out: &mut [u8]) {
        match self {
            Sample::Int16 => out[..2].copy_from_slice(&value.to_le_bytes()),
            Sample::Int32 { .. } => {
                // Alineado a la izquierda: los 16 bits útiles en lo alto, da igual cuántos sean
                // válidos (los de abajo quedan a cero).
                out[..4].copy_from_slice(&(i32::from(value) << 16).to_le_bytes());
            }
            Sample::Float32 => {
                out[..4].copy_from_slice(&(f32::from(value) / 32_768.0).to_le_bytes());
            }
        }
    }

    /// Lee una muestra de `bytes` (al menos [`Sample::bytes`]).
    fn read(self, bytes: &[u8]) -> i16 {
        match self {
            Sample::Int16 => i16::from_le_bytes([bytes[0], bytes[1]]),
            Sample::Int32 { .. } => {
                // Los 16 bits altos: los válidos están alineados a la izquierda.
                (i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) >> 16) as i16
            }
            Sample::Float32 => {
                let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                (value * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16
            }
        }
    }
}

/// Formato de las tramas del dispositivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceFormat {
    /// Tipo de muestra.
    pub(crate) sample: Sample,
    /// Canales intercalados del dispositivo.
    pub(crate) channels: usize,
}

impl DeviceFormat {
    /// El nuestro: `i16` con `channels` canales (la conversión es una copia).
    pub(crate) const fn pcm16(channels: usize) -> Self {
        Self {
            sample: Sample::Int16,
            channels,
        }
    }

    /// Bytes de una trama (una muestra por canal).
    pub(crate) const fn bytes_per_frame(&self) -> usize {
        self.channels * self.sample.bytes()
    }

    /// Escribe en `out` las tramas de `pcm` (`channels` canales nuestros, intercalados) en el
    /// formato del dispositivo. Devuelve los bytes escritos: las tramas que cupieron en `out`
    /// (tramas × [`DeviceFormat::bytes_per_frame`]), nunca más de lo que hay en él.
    pub(crate) fn encode(&self, pcm: &[i16], channels: usize, out: &mut [u8]) -> usize {
        let channels = channels.max(1);
        let frame_bytes = self.bytes_per_frame();
        let sample_bytes = self.sample.bytes();
        let mut frames = 0;
        for (frame, bytes) in pcm
            .chunks_exact(channels)
            .zip(out.chunks_exact_mut(frame_bytes))
        {
            frames += 1;
            for (k, slot) in bytes.chunks_exact_mut(sample_bytes).enumerate() {
                let value = match (channels, self.channels) {
                    (ours, theirs) if ours == theirs => frame[k],
                    // Estéreo a mono: la media.
                    (2, 1) => ((i32::from(frame[0]) + i32::from(frame[1])) / 2) as i16,
                    // Mono a varios: los dos frontales; estéreo a varios: los dos primeros.
                    (1, _) if k < 2 => frame[0],
                    (_, _) if k < frame.len() => frame[k],
                    _ => 0,
                };
                self.sample.write(value, slot);
            }
        }
        frames * frame_bytes
    }

    /// Lee las tramas completas de `bytes` y deja en `out` sus muestras en `channels` canales
    /// nuestros, intercaladas. Devuelve las tramas leídas.
    pub(crate) fn decode(&self, bytes: &[u8], channels: usize, out: &mut Vec<i16>) -> usize {
        let channels = channels.max(1);
        let frame_bytes = self.bytes_per_frame();
        let sample_bytes = self.sample.bytes();
        out.clear();
        let mut frames = 0;
        for frame in bytes.chunks_exact(frame_bytes) {
            frames += 1;
            let theirs = |k: usize| self.sample.read(&frame[k * sample_bytes..]);
            match (channels, self.channels) {
                (ours, device) if ours == device => out.extend((0..ours).map(theirs)),
                // A mono: la media de todos los canales del dispositivo.
                (1, device) => {
                    let sum: i32 = (0..device).map(|k| i32::from(theirs(k))).sum();
                    out.push((sum / device as i32) as i16);
                }
                // Mono a estéreo: el mismo en los dos; más canales: los dos primeros.
                (_, 1) => out.extend(std::iter::repeat_n(theirs(0), channels)),
                (ours, _) => out.extend((0..ours).map(theirs)),
            }
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_is_a_plain_copy() {
        let format = DeviceFormat::pcm16(2);
        assert_eq!(format.bytes_per_frame(), 4);
        let pcm = [1i16, -2, 3, -4];
        let mut bytes = [0u8; 8];
        assert_eq!(format.encode(&pcm, 2, &mut bytes), 8);
        assert_eq!(bytes, [1, 0, 0xFE, 0xFF, 3, 0, 0xFC, 0xFF]);
        let mut back = Vec::new();
        assert_eq!(format.decode(&bytes, 2, &mut back), 2);
        assert_eq!(back, pcm);
    }

    #[test]
    fn float_and_wide_integers_round_trip() {
        for sample in [
            Sample::Float32,
            Sample::Int32 { valid_bits: 24 },
            Sample::Int32 { valid_bits: 32 },
        ] {
            let format = DeviceFormat {
                sample,
                channels: 2,
            };
            assert_eq!(format.bytes_per_frame(), 8);
            let pcm = [0i16, 32_767, -32_768, 1_234];
            let mut bytes = [0u8; 16];
            assert_eq!(format.encode(&pcm, 2, &mut bytes), 16);
            let mut back = Vec::new();
            assert_eq!(format.decode(&bytes, 2, &mut back), 2);
            assert_eq!(back, pcm, "{sample:?}");
        }
        // Un flotante fuera de rango se satura.
        let loud = 2.0f32.to_le_bytes();
        let mut back = Vec::new();
        DeviceFormat {
            sample: Sample::Float32,
            channels: 1,
        }
        .decode(&loud, 1, &mut back);
        assert_eq!(back, [32_767]);
    }

    #[test]
    fn channels_are_spread_and_folded() {
        let six = DeviceFormat {
            sample: Sample::Float32,
            channels: 6,
        };
        // Mono nuestro a 5.1: los dos frontales, el resto en silencio.
        let mut bytes = [0u8; 24];
        assert_eq!(six.encode(&[8_000], 1, &mut bytes), 24);
        let mut back = Vec::new();
        assert_eq!(six.decode(&bytes, 2, &mut back), 1);
        assert_eq!(back, [8_000, 8_000]);
        let floats: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert!(floats[2..].iter().all(|v| *v == 0.0), "{floats:?}");
        // Estéreo nuestro a 5.1 y vuelta.
        assert_eq!(six.encode(&[100, -100], 2, &mut bytes), 24);
        assert_eq!(six.decode(&bytes, 2, &mut back), 1);
        assert_eq!(back, [100, -100]);
        // Captura de 5.1 a mono: la media de los seis.
        assert_eq!(six.decode(&bytes, 1, &mut back), 1);
        assert_eq!(back, [0]);
        // Estéreo nuestro a un dispositivo mono: la media; mono del dispositivo a estéreo: doble.
        let mono = DeviceFormat::pcm16(1);
        let mut two = [0u8; 2];
        assert_eq!(mono.encode(&[1_000, 3_000], 2, &mut two), 2);
        assert_eq!(mono.decode(&two, 2, &mut back), 1);
        assert_eq!(back, [2_000, 2_000]);
    }

    #[test]
    fn partial_frames_are_ignored() {
        let format = DeviceFormat::pcm16(2);
        let mut back = Vec::new();
        assert_eq!(format.decode(&[1, 0, 2, 0, 3], 2, &mut back), 1);
        assert_eq!(back, [1, 2]);
        let mut small = [0u8; 4];
        assert_eq!(
            format.encode(&[1, 2, 3, 4], 2, &mut small),
            4,
            "solo cabe una trama"
        );
        assert_eq!(small, [1, 0, 2, 0]);
        let mut odd = [0u8; 7];
        assert_eq!(format.encode(&[1, 2, 3, 4], 2, &mut odd), 4, "trama entera");
    }
}
