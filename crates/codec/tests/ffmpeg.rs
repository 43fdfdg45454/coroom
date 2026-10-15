//! Códecs de FFmpeg por software (los que hay en la CI): ida y vuelta AV1 con SVT-AV1 → dav1d,
//! H.264 de FFmpeg intercambiable con OpenH264, cambio de bitrate y peticiones de intra.

#![cfg(feature = "ffmpeg")]

use coroom_codec::convert::bgra_to_i420;
use coroom_codec::ffmpeg::{FfmpegDecoder, FfmpegEncoder};
use coroom_codec::openh264::{OpenH264Decoder, OpenH264Encoder};
use coroom_codec::{
    Backend, Codec, Decoder, DecoderBackend, EncodedSlice, Encoder, EncoderConfig, PixelFormat,
    RawFrame,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

/// Patrón con movimiento: degradado que se desplaza y un cuadrado que avanza.
fn pattern(frame_id: u64) -> RawFrame {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let shift = frame_id as usize * 4;
    let mut data = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let at = (y * w + x) * 4;
            let r = ((x + shift) * 255 / w) as u8;
            let g = (y * 255 / h) as u8;
            let b = (((x + y + shift) / 4) % 256) as u8;
            data[at..at + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
    let side = 64;
    let x0 = (frame_id as usize * 9) % (w - side);
    let y0 = (frame_id as usize * 5) % (h - side);
    for y in y0..y0 + side {
        for x in x0..x0 + side {
            let at = (y * w + x) * 4;
            data[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
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

fn config(codec: Codec) -> EncoderConfig {
    EncoderConfig {
        codec,
        width: WIDTH,
        height: HEIGHT,
        fps: 30,
        bitrate_kbps: 4_000,
        slices: 1,
        format: coroom_codec::VideoFormat::SDR_420,
    }
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
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
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// Codifica 30 fotogramas con `encoder`, los decodifica con `decoder` y devuelve el peor PSNR del
/// plano Y (cada fotograma debe salir en su llamada: baja latencia).
fn round_trip(encoder: &mut dyn Encoder, decoder: &mut dyn Decoder) -> f64 {
    let mut worst = f64::INFINITY;
    for frame_id in 0..30 {
        let source = pattern(frame_id);
        let reference = bgra_to_i420(&source).expect("conversión");
        let slices = encoder.encode(&source).expect("codificación");
        assert!(
            !slices.is_empty(),
            "el fotograma {frame_id} sale en su llamada"
        );
        assert!(slices.iter().all(|slice| slice.frame_id == frame_id));
        if frame_id == 0 {
            assert!(
                slices.iter().any(|slice| slice.keyframe),
                "el primero es intra"
            );
        }
        let decoded = decoder
            .decode(&slices)
            .expect("decodificación")
            .expect("imagen");
        assert_eq!((decoded.width, decoded.height), (WIDTH, HEIGHT));
        let (y, _, _) = reference.i420_planes().expect("planos");
        worst = worst.min(psnr(decoded.luma().expect("luma"), y));
    }
    worst
}

#[test]
fn av1_svt_to_dav1d_round_trip_has_good_psnr() {
    let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).unwrap();
    encoder.configure(&config(Codec::Av1)).unwrap();
    assert_eq!(
        encoder.unused_options(),
        &[] as &[String],
        "SVT-AV1 reconoce todas las opciones"
    );
    let mut decoder = FfmpegDecoder::new(Codec::Av1, DecoderBackend::Software).unwrap();
    let worst = round_trip(&mut encoder, &mut decoder);
    println!("AV1 SVT-AV1 → dav1d: PSNR Y mínimo {worst:.2} dB");
    assert!(worst > 30.0, "PSNR mínimo del plano Y: {worst:.2} dB");
}

#[test]
fn h264_from_ffmpeg_and_openh264_are_interchangeable() {
    // OpenH264 → `h264` de FFmpeg.
    let mut encoder = OpenH264Encoder::new();
    encoder.configure(&config(Codec::H264)).unwrap();
    let mut decoder = FfmpegDecoder::new(Codec::H264, DecoderBackend::Software).unwrap();
    let worst = round_trip(&mut encoder, &mut decoder);
    println!("H.264 OpenH264 → h264 de FFmpeg: PSNR Y mínimo {worst:.2} dB");
    assert!(worst > 30.0, "{worst:.2} dB");

    // Y lo mismo con OpenH264 al decodificar, para comparar.
    let mut encoder = OpenH264Encoder::new();
    encoder.configure(&config(Codec::H264)).unwrap();
    let mut decoder = OpenH264Decoder::new().unwrap();
    let reference = round_trip(&mut encoder, &mut decoder);
    assert!(
        (worst - reference).abs() < 0.5,
        "{worst:.2} frente a {reference:.2} dB"
    );
}

#[test]
fn svt_av1_bitrate_changes_reopen_and_restart_with_an_intra() {
    let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).unwrap();
    encoder.configure(&config(Codec::Av1)).unwrap();
    let mut decoder = FfmpegDecoder::new(Codec::Av1, DecoderBackend::Software).unwrap();
    for frame_id in 0..5 {
        let slices = encoder.encode(&pattern(frame_id)).unwrap();
        assert!(decoder.decode(&slices).unwrap().is_some());
    }
    // SVT-AV1 en FFmpeg 8.1 solo lee el bitrate al abrir: se reabre y sale un intra.
    encoder.set_bitrate(1_500).unwrap();
    assert_eq!(encoder.reopened(), 1);
    let after = encoder.encode(&pattern(5)).unwrap();
    assert!(after.iter().any(|slice| slice.keyframe));
    decoder.reset();
    assert!(decoder.decode(&after).unwrap().is_some());
    // El mismo bitrate no reabre.
    encoder.set_bitrate(1_500).unwrap();
    assert_eq!(encoder.reopened(), 1);
}

#[test]
fn svt_av1_honours_keyframe_requests() {
    let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).unwrap();
    encoder.configure(&config(Codec::Av1)).unwrap();
    let keyframe = |slices: &[EncodedSlice]| slices.iter().any(|slice| slice.keyframe);
    assert!(keyframe(&encoder.encode(&pattern(0)).unwrap()));
    assert!(
        !keyframe(&encoder.encode(&pattern(1)).unwrap()),
        "sin petición, P"
    );
    encoder.request_keyframe();
    assert!(
        keyframe(&encoder.encode(&pattern(2)).unwrap()),
        "request_keyframe da intra"
    );
    assert!(!keyframe(&encoder.encode(&pattern(3)).unwrap()));
}

#[test]
fn ffmpeg_encoders_accept_i420_and_new_sizes() {
    let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).unwrap();
    encoder.configure(&config(Codec::Av1)).unwrap();
    let i420 = bgra_to_i420(&pattern(0)).unwrap();
    assert!(!encoder.encode(&i420).unwrap().is_empty());
    // Un tamaño distinto del configurado (p. ej., la pantalla real) reabre el codificador.
    let mut small = bgra_to_i420(&pattern(1)).unwrap();
    small.width = 320;
    small.height = 180;
    small.stride = 320;
    small.data.truncate(RawFrame::i420_len(320, 180));
    let slices = encoder.encode(&small).unwrap();
    assert!(slices.iter().any(|slice| slice.keyframe));
    assert_eq!(encoder.reopened(), 1);
    let mut decoder = FfmpegDecoder::new(Codec::Av1, DecoderBackend::Software).unwrap();
    let decoded = decoder.decode(&slices).unwrap().expect("imagen");
    assert_eq!((decoded.width, decoded.height), (320, 180));
}

/// HDR10 de extremo a extremo por software: SVT-AV1 a 10 bits con BT.2020 PQ → dav1d,
/// que entrega P010 marcado como HDR10; la luma decodificada se parece a la de la conversión.
#[test]
fn av1_hdr10_round_trip_keeps_ten_bits_and_pq() {
    use coroom_codec::{ColorSpace, VideoFormat};

    let mut encoder = FfmpegEncoder::new(Codec::Av1, Backend::Software).unwrap();
    encoder
        .configure(&EncoderConfig {
            format: VideoFormat::HDR10_420,
            ..config(Codec::Av1)
        })
        .unwrap();
    assert!(encoder.unused_options().is_empty());
    let mut decoder = FfmpegDecoder::new(Codec::Av1, DecoderBackend::Software).unwrap();
    let mut worst = f64::INFINITY;
    let word = |plane: &[u8], i: usize| {
        f64::from(u16::from_le_bytes([plane[2 * i], plane[2 * i + 1]]) >> 6)
    };
    for frame_id in 0..10 {
        let source = pattern(frame_id);
        let reference = coroom_codec::convert::to_p010(&source).unwrap();
        let slices = encoder.encode(&source).unwrap();
        let Some(decoded) = decoder.decode(&slices).unwrap() else {
            continue;
        };
        assert_eq!(decoded.format, PixelFormat::P010);
        assert_eq!(decoded.color, ColorSpace::HDR10, "el VUI dice BT.2020 PQ");
        let (y, _) = decoded.p010_planes().unwrap();
        let (y_ref, _) = reference.p010_planes().unwrap();
        let n = (WIDTH * HEIGHT) as usize;
        let mse = (0..n)
            .map(|i| (word(y, i) - word(y_ref, i)).powi(2))
            .sum::<f64>()
            / n as f64;
        let psnr = 10.0 * (1023.0f64 * 1023.0 / mse.max(1e-9)).log10();
        worst = worst.min(psnr);
    }
    println!("AV1 HDR10 SVT-AV1 → dav1d: PSNR Y mínimo {worst:.2} dB");
    assert!(worst > 30.0 && worst.is_finite(), "{worst:.2} dB");
}

/// 4:4:4 decodificable: dav1d declara 4:4:4 y el `hevc` de FFmpeg también (la tabla de `caps`),
/// y ningún codificador por software lo produce.
#[test]
fn format_tables_match_the_software_codecs() {
    use coroom_codec::{VideoFormat, decoder_supports, encoder_supports};

    assert!(decoder_supports(
        Codec::Av1,
        DecoderBackend::Software,
        VideoFormat::SDR_444
    ));
    assert!(decoder_supports(
        Codec::Hevc,
        DecoderBackend::Software,
        VideoFormat::HDR10_420
    ));
    assert!(!decoder_supports(
        Codec::H264,
        DecoderBackend::Software,
        VideoFormat::SDR_444
    ));
    assert!(!encoder_supports(
        Codec::Av1,
        Backend::Software,
        VideoFormat::SDR_444
    ));
    assert!(!encoder_supports(
        Codec::H264,
        Backend::Software,
        VideoFormat::HDR10_420
    ));
    assert!(encoder_supports(
        Codec::Av1,
        Backend::Software,
        VideoFormat::HDR10_420
    ));
    assert!(encoder_supports(
        Codec::Hevc,
        Backend::Nvenc,
        VideoFormat::SDR_444
    ));
    assert!(!encoder_supports(
        Codec::Av1,
        Backend::Nvenc,
        VideoFormat::SDR_444
    ));
}
