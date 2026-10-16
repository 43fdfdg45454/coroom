//! Comparación medida de los modos de tasa ([`RateMode`]): tasa constante (lo de siempre) frente
//! a calidad fija con techo, con el mismo techo, en cada codificador que haya en esta máquina.
//!
//! Diez segundos a 1080p60 en cuatro tramos de 2,5 s: escritorio quieto, movimiento, ráfaga de
//! «partículas» (ruido en el 40 % de la imagen, como los efectos de OSU) y quieto otra vez. Por
//! tramo: lo que gasta (kbit/s), el fotograma más grande (lo que tarda en salir es latencia) y el
//! PSNR de la luma decodificada frente a la original.
//!
//! `cargo test --release -p coroom-codec --test rate_modes -- --ignored --nocapture`
//! (`RATE_CAP_KBPS` cambia el techo, por defecto 20 000; `RATE_ONLY=Vaapi` mide solo esos).

#![cfg(feature = "ffmpeg")]

use coroom_codec::convert::bgra_to_i420;
use coroom_codec::ffmpeg::{FfmpegDecoder, FfmpegEncoder};
use coroom_codec::{
    Backend, Codec, Decoder, DecoderBackend, Encoder, EncoderConfig, PixelFormat, RateMode,
    RawFrame,
};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 60;
/// Fotogramas por tramo (2,5 s).
const PHASE: u64 = 150;
const PHASES: [&str; 4] = ["quieto", "movimiento", "partículas", "quieto"];

/// Generador determinista (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Un fotograma del tramo que toca: un «escritorio» (degradado con franjas finas, como texto),
/// que se desplaza en el movimiento, con ruido en bloques en las partículas.
fn frame(frame_id: u64, rng: &mut Rng) -> RawFrame {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let phase = (frame_id / PHASE) as usize;
    let shift = match phase {
        1 | 2 => (frame_id - PHASE) as usize * 6,
        _ => 0,
    };
    let mut data = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let at = (y * w + x) * 4;
            let xs = x + shift;
            let stripe = if (y / 3) % 7 == 0 && (xs / 5) % 3 != 0 {
                60
            } else {
                0
            };
            let r = ((xs * 200 / w) as u8).saturating_add(stripe);
            let g = ((y * 200 / h) as u8).saturating_add(stripe);
            let b = (((xs + y) / 6) % 200) as u8;
            data[at..at + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
    if phase == 2 {
        // Partículas: bloques de 16×16 de ruido en el 40 % de la imagen, distintos cada vez.
        for by in (0..h).step_by(16) {
            for bx in (0..w).step_by(16) {
                if rng.next() % 10 >= 4 {
                    continue;
                }
                for y in by..(by + 16).min(h) {
                    for x in bx..(bx + 16).min(w) {
                        let at = (y * w + x) * 4;
                        let v = rng.next();
                        data[at..at + 3].copy_from_slice(&[
                            v as u8,
                            (v >> 8) as u8,
                            (v >> 16) as u8,
                        ]);
                    }
                }
            }
        }
    }
    RawFrame {
        frame_id,
        width: WIDTH,
        height: HEIGHT,
        format: PixelFormat::Bgra,
        stride: WIDTH * 4,
        data,
        gpu: None,
        color: coroom_codec::ColorSpace::SDR,
        pool: None,
    }
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = f64::from(x) - f64::from(y);
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// Por tramo: bytes, fotograma más grande y suma de PSNR (con cuántos).
#[derive(Default, Clone, Copy)]
struct Phase {
    bytes: u64,
    largest: usize,
    psnr: f64,
    measured: u32,
}

fn run(
    codec: Codec,
    backend: Backend,
    mode: RateMode,
    cap: u32,
) -> Result<([Phase; 4], String), String> {
    let mut encoder = FfmpegEncoder::new(codec, backend).map_err(|err| err.to_string())?;
    encoder.set_rate_mode(mode);
    encoder
        .configure(&EncoderConfig {
            codec,
            width: WIDTH,
            height: HEIGHT,
            fps: FPS,
            bitrate_kbps: cap,
            slices: 1,
            format: coroom_codec::VideoFormat::SDR_420,
        })
        .map_err(|err| err.to_string())?;
    let mut decoder =
        FfmpegDecoder::new(codec, DecoderBackend::Software).map_err(|err| err.to_string())?;
    let mut rng = Rng(0x5EED);
    let mut phases = [Phase::default(); 4];
    for frame_id in 0..PHASE * 4 {
        let source = frame(frame_id, &mut rng);
        let slices = encoder.encode(&source).map_err(|err| err.to_string())?;
        let size: usize = slices.iter().map(|slice| slice.data.len()).sum();
        let phase = &mut phases[(frame_id / PHASE) as usize];
        phase.bytes += size as u64;
        // El primer fotograma (intra de arranque) no cuenta para el más grande.
        if frame_id > 0 {
            phase.largest = phase.largest.max(size);
        }
        if slices.is_empty() {
            continue;
        }
        // PSNR de uno de cada diez (la decodificación por software a 1080p es lenta). Si el
        // decodificador no puede con el flujo, se mide solo lo que gasta.
        let decoded = decoder.decode(&slices).ok().flatten();
        if frame_id % 10 == 5
            && let Some(decoded) = decoded
        {
            let reference = bgra_to_i420(&source).map_err(|err| err.to_string())?;
            let (y, _, _) = reference.i420_planes().ok_or("planos")?;
            phase.psnr += psnr(decoded.luma().ok_or("luma")?, y);
            phase.measured += 1;
        }
    }
    let opened = encoder
        .rate_control_info()
        .map(ToString::to_string)
        .unwrap_or_default();
    Ok((phases, opened))
}

#[test]
#[ignore = "medida larga con los codificadores de esta máquina"]
fn constant_rate_against_capped_quality() {
    let cap: u32 = std::env::var("RATE_CAP_KBPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let seconds = PHASE as f64 / f64::from(FPS);
    println!(
        "Techo {cap} kbit/s, 1080p{FPS}; por tramo: kbit/s · fotograma mayor (KB) · PSNR luma"
    );
    for (codec, backend) in [
        (Codec::Av1, Backend::Nvenc),
        (Codec::Hevc, Backend::Nvenc),
        (Codec::H264, Backend::Nvenc),
        (Codec::Av1, Backend::Software),
        (Codec::Av1, Backend::VulkanVideo),
        (Codec::Hevc, Backend::VulkanVideo),
        (Codec::H264, Backend::VulkanVideo),
        (Codec::Av1, Backend::Vaapi),
        (Codec::Hevc, Backend::Vaapi),
        (Codec::H264, Backend::Vaapi),
        (Codec::Av1, Backend::Amf),
        (Codec::Av1, Backend::Qsv),
    ] {
        // `RATE_ONLY=VulkanVideo` (o `Hevc/Vaapi`) mide solo esos.
        if std::env::var("RATE_ONLY")
            .is_ok_and(|only| !format!("{codec:?}/{backend:?}").contains(&only))
        {
            continue;
        }
        for mode in [RateMode::Constant, RateMode::Capped] {
            match run(codec, backend, mode, cap) {
                Ok((phases, opened)) => {
                    let line: Vec<String> = phases
                        .iter()
                        .zip(PHASES)
                        .map(|(phase, name)| {
                            format!(
                                "{name} {:>6.0} · {:>4.0} · {:>4.1}",
                                phase.bytes as f64 * 8.0 / 1000.0 / seconds,
                                phase.largest as f64 / 1000.0,
                                phase.psnr / f64::from(phase.measured.max(1))
                            )
                        })
                        .collect();
                    let total: u64 = phases.iter().map(|phase| phase.bytes).sum();
                    println!(
                        "{codec:?}/{backend:?} {mode:?}: {} | media {:.0} kbit/s | {opened}",
                        line.join(" | "),
                        total as f64 * 8.0 / 1000.0 / (seconds * 4.0)
                    );
                }
                Err(err) => println!("{codec:?}/{backend:?} {mode:?}: no se pudo ({err})"),
            }
        }
    }
}
