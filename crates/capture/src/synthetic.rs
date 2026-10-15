//! Capturador sintético: un patrón con movimiento en todo el fotograma a cadencia fija.
//!
//! Es la fuente de la CI, de `coroom probe` sin pantalla y de la futura prueba A/B del cifrado.
//! El patrón combina tres elementos:
//!
//! - ocho bandas horizontales con un degradado que se desplaza a una velocidad distinta en cada una
//!   (todo el fotograma cambia en cada tick, así que el codificador trabaja de verdad);
//! - un bloque blanco que rebota contra los bordes;
//! - una fila de 32 barras arriba a la izquierda con los bits del contador de fotograma, blancas
//!   para 1 y negras para 0, que permite leer el id en una captura de la ventana.
//!
//! El cursor no se dibuja en la imagen: sigue su propia trayectoria y viaja como metadatos, con una
//! forma fija pequeña ([`cursor_shape`]) para probar el cursor por la red en la CI.
//!
//! Con HDR pedido ([`Capturer::set_options`]) entrega [`FramePixels::Rgba16F`] en scRGB:
//! el patrón SDR en el blanco de referencia de 203 nits y el bloque que rebota a 1000 nits, para
//! probar el camino HDR de extremo a extremo sin pantalla HDR.
//!
//! Todo se dibuja copiando filas precalculadas, así que cuesta poco incluso en debug. El fotograma
//! se genera poco antes del tick (con un margen del doble de lo que tardó el anterior) y se entrega
//! al llegar: `captured_at` es el instante del tick.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::{
    CaptureError, CaptureOptions, CaptureTarget, CapturedFrame, Capturer, CursorShape, CursorState,
    FramePixels,
};

/// Luminancia del bloque en HDR, en scRGB (1,0 = 80 nits): 1000 nits.
pub const HDR_BLOCK_SCRGB: f32 = 12.5;

/// Blanco de referencia del SDR dentro del HDR (BT.2408, 203 nits) en scRGB.
const SDR_WHITE_SCRGB: f32 = 203.0 / 80.0;

/// `f32` normal y positivo (o cero) → `f16` (solo para las tablas del patrón HDR).
fn half_bits(value: f32) -> u16 {
    if value <= 0.0 {
        return 0;
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    if exponent <= 0 {
        return 0;
    }
    let mantissa = ((bits & 0x7f_ffff) + 0x1000) >> 13;
    (((exponent as u32) << 10) + mantissa).min(0x7bff) as u16
}

/// sRGB de 8 bits → scRGB lineal en el blanco de referencia, en `f16`.
fn hdr_table() -> [u16; 256] {
    std::array::from_fn(|i| {
        let v = i as f32 / 255.0;
        let linear = if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        };
        half_bits(linear * SDR_WHITE_SCRGB)
    })
}

/// Identificador de la forma fija del cursor sintético.
pub const CURSOR_SHAPE_ID: u64 = 1;

/// Lado de la forma del cursor sintético.
pub const CURSOR_SIDE: u32 = 12;

/// Forma fija del cursor sintético: una flecha de 12 × 12 blanca con borde negro y el punto
/// activo en la punta (arriba a la izquierda).
pub fn cursor_shape() -> CursorShape {
    let side = CURSOR_SIDE as usize;
    let mut rgba = vec![0u8; side * side * 4];
    for y in 0..side {
        for x in 0..side {
            // Triángulo con la punta en (0, 0): dentro si x ≤ y, por encima de la diagonal.
            if x > y || (y >= side - 1 && x > 0) {
                continue;
            }
            let edge = x == 0 || x == y || y == side - 2;
            let colour = if edge { 0 } else { 255 };
            rgba[(y * side + x) * 4..][..4].copy_from_slice(&[colour, colour, colour, 255]);
        }
    }
    CursorShape {
        shape_id: CURSOR_SHAPE_ID,
        width: CURSOR_SIDE,
        height: CURSOR_SIDE,
        hotspot_x: 0,
        hotspot_y: 0,
        rgba,
    }
}

/// Nombre del único objetivo del capturador sintético.
pub const TARGET_NAME: &str = "Sintético";

const BANDS: usize = 8;
const PERIOD: usize = 256;
const COUNTER_BITS: usize = 32;

/// Holgura sobre el doble del último dibujo para tenerlo listo antes del tick.
const RENDER_MARGIN: Duration = Duration::from_millis(1);

/// Posición que va y vuelve entre 0 y `span` avanzando una unidad por paso.
fn bounce(step: u64, span: usize) -> usize {
    if span == 0 {
        return 0;
    }
    let span = span as u64;
    let phase = step % (2 * span);
    (if phase < span {
        phase
    } else {
        2 * span - phase
    }) as usize
}

/// Capturador de un patrón sintético en movimiento.
#[derive(Debug)]
pub struct SyntheticCapturer {
    width: u32,
    height: u32,
    period: Duration,
    /// Filas base de cada banda: `width + PERIOD` píxeles BGRA.
    band_rows: Vec<Vec<u8>>,
    white_row: Vec<u8>,
    black_row: Vec<u8>,
    next_tick: Option<Instant>,
    next_id: u64,
    /// Lo que tardó el último dibujo.
    render_cost: Duration,
    cursor_shape: Arc<CursorShape>,
    /// Con HDR, la tabla sRGB → `f16`.
    hdr: Option<Box<[u16; 256]>>,
}

impl SyntheticCapturer {
    /// Crea un capturador de `width × height` a `fps` fotogramas por segundo.
    ///
    /// Los valores nulos se elevan a 1 para no dividir por cero.
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let fps = fps.max(1);
        let row_len = width as usize + PERIOD;
        let band_rows = (0..BANDS)
            .map(|band| {
                let mut row = Vec::with_capacity(row_len * 4);
                for x in 0..row_len {
                    let t = (x % PERIOD) as u8;
                    let tint = (band * 255 / (BANDS - 1)) as u8;
                    // B, G, R, A.
                    row.extend_from_slice(&[tint, 255 - t, t, 255]);
                }
                row
            })
            .collect();
        Self {
            width,
            height,
            period: Duration::from_secs(1) / fps,
            band_rows,
            white_row: [255u8, 255, 255, 255].repeat(width as usize),
            black_row: [0u8, 0, 0, 255].repeat(width as usize),
            next_tick: None,
            next_id: 0,
            render_cost: Duration::ZERO,
            cursor_shape: Arc::new(cursor_shape()),
            hdr: None,
        }
    }

    /// Pasa el patrón SDR (BGRA) a RGBA de medio flotante en scRGB, con el bloque a
    /// [`HDR_BLOCK_SCRGB`].
    fn to_hdr(&self, table: &[u16; 256], bgra: &[u8], frame_id: u64) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = Vec::with_capacity(w * h * 8);
        for px in bgra.as_chunks::<4>().0 {
            for c in [px[2], px[1], px[0]] {
                out.extend_from_slice(&table[usize::from(c)].to_le_bytes());
            }
            out.extend_from_slice(&half_bits(1.0).to_le_bytes());
        }
        let side = (h / 6).max(1);
        let (bx, by) = self.block_position(frame_id, side);
        let bright = half_bits(HDR_BLOCK_SCRGB).to_le_bytes();
        for y in by..(by + side).min(h) {
            for x in bx..(bx + side).min(w) {
                let at = (y * w + x) * 8;
                for c in 0..3 {
                    out[at + 2 * c..at + 2 * c + 2].copy_from_slice(&bright);
                }
            }
        }
        out
    }

    /// Ancho del fotograma.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Alto del fotograma.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Intervalo entre fotogramas.
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Posición del bloque que rebota en el fotograma `frame_id` (esquina superior izquierda).
    fn block_position(&self, frame_id: u64, side: usize) -> (usize, usize) {
        let w = self.width as usize;
        let h = self.height as usize;
        (
            bounce(frame_id * 7, w.saturating_sub(side)),
            bounce(frame_id * 5, h.saturating_sub(side)),
        )
    }

    /// Posición del cursor sintético en el fotograma `frame_id`.
    fn cursor_position(&self, frame_id: u64) -> (usize, usize) {
        let w = self.width as usize;
        let h = self.height as usize;
        (
            bounce(frame_id * 11 + 40, w.saturating_sub(1)),
            bounce(frame_id * 3 + 90, h.saturating_sub(1)),
        )
    }

    /// Dibuja el fotograma `frame_id`.
    pub fn render(&self, frame_id: u64) -> (Vec<u8>, CursorState) {
        let w = self.width as usize;
        let h = self.height as usize;
        let stride = w * 4;
        // Del pool de la captura: todas las filas se escriben a continuación.
        let mut data = crate::pool::take(stride * h);

        for (y, row) in data.chunks_exact_mut(stride).enumerate() {
            let band = y * BANDS / h;
            let speed = band as u64 + 1;
            let shift = ((frame_id * speed * 2) % PERIOD as u64) as usize;
            row.copy_from_slice(&self.band_rows[band][shift * 4..(shift + w) * 4]);
        }

        let fill = |data: &mut [u8], x0: usize, y0: usize, x1: usize, y1: usize, src: &[u8]| {
            let (x1, y1) = (x1.min(w), y1.min(h));
            if x0 >= x1 {
                return;
            }
            for y in y0..y1 {
                let at = y * stride + x0 * 4;
                data[at..at + (x1 - x0) * 4].copy_from_slice(&src[..(x1 - x0) * 4]);
            }
        };

        let side = (h / 6).max(1);
        let (bx, by) = self.block_position(frame_id, side);
        fill(&mut data, bx, by, bx + side, by + side, &self.white_row);

        let bar_w = (w / 64).max(1);
        let bar_h = (h / 16).max(1);
        for bit in 0..COUNTER_BITS {
            let src = if frame_id >> (COUNTER_BITS - 1 - bit) & 1 == 1 {
                &self.white_row
            } else {
                &self.black_row
            };
            let x0 = bit * bar_w;
            // Una columna negra de separación entre barras.
            fill(
                &mut data,
                x0,
                0,
                x0 + bar_w.saturating_sub(1).max(1),
                bar_h,
                src,
            );
        }

        // El cursor sigue su propia trayectoria para que se distinga del bloque.
        let (cx, cy) = self.cursor_position(frame_id);
        let cursor = CursorState {
            x: cx as i32,
            y: cy as i32,
            visible: true,
            shape_id: CURSOR_SHAPE_ID,
        };
        (data, cursor)
    }
}

impl Capturer for SyntheticCapturer {
    fn targets(&self) -> Result<Vec<CaptureTarget>, CaptureError> {
        Ok(vec![CaptureTarget::Monitor {
            id: 0,
            name: TARGET_NAME.to_owned(),
        }])
    }

    fn start(&mut self, _target: &CaptureTarget) -> Result<(), CaptureError> {
        self.next_tick = Some(Instant::now() + self.period);
        Ok(())
    }

    fn next_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        let Some(tick) = self.next_tick else {
            return Err(CaptureError::Backend("la captura no está iniciada".into()));
        };
        let frame_id = self.next_id;
        // Se dibuja justo antes del tick, no nada más entregar el anterior: así el dibujo no compite
        // por la CPU con la codificación del fotograma que acaba de salir.
        let budget = self.render_cost.saturating_mul(2) + RENDER_MARGIN;
        if let Some(start) = tick.checked_sub(budget) {
            let now = Instant::now();
            if now < start {
                thread::sleep(start - now);
            }
        }
        let render_start = Instant::now();
        let (data, cursor) = self.render(frame_id);
        let pixels = match &self.hdr {
            Some(table) => FramePixels::Rgba16F {
                data: self.to_hdr(table, &data, frame_id),
                stride: self.width * 8,
            },
            None => FramePixels::Bgra {
                data,
                stride: self.width * 4,
            },
        };
        self.render_cost = render_start.elapsed();

        let now = Instant::now();
        let captured_at = if now < tick {
            thread::sleep(tick - now);
            self.next_tick = Some(tick + self.period);
            tick
        } else {
            // Vamos tarde (el consumidor tardó más de un periodo): se saltan los ticks perdidos y la
            // cadencia se reancla al ahora para no entregar fotogramas con fecha pasada.
            let behind = now - tick;
            let missed = behind.as_nanos() / self.period.as_nanos().max(1);
            let skip = u32::try_from(missed + 1).unwrap_or(u32::MAX);
            self.next_tick = Some(tick + self.period.saturating_mul(skip));
            now
        };
        self.next_id += 1;
        Ok(CapturedFrame {
            frame_id,
            width: self.width,
            height: self.height,
            captured_at,
            cursor: Some(cursor),
            cursor_shape: Some(Arc::clone(&self.cursor_shape)),
            pixels,
            // El patrón nace en CPU: no se copia de ninguna parte.
            cpu_copies: 0,
        })
    }

    fn stop(&mut self) {
        self.next_tick = None;
    }

    /// Con `hdr`, el patrón sale en scRGB de medio flotante. La cadencia es la del constructor.
    fn set_options(&mut self, options: CaptureOptions) -> CaptureOptions {
        self.hdr = options.hdr.then(|| Box::new(hdr_table()));
        options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(data: &[u8], width: u32, x: usize, y: usize) -> [u8; 4] {
        let at = (y * width as usize + x) * 4;
        [data[at], data[at + 1], data[at + 2], data[at + 3]]
    }

    #[test]
    fn hdr_frames_are_half_float_with_a_bright_block() {
        let mut capturer = SyntheticCapturer::new(64, 48, 120);
        let options = CaptureOptions {
            max_fps: 120,
            hdr: true,
        };
        assert_eq!(capturer.set_options(options), options);
        let target = capturer.targets().unwrap().remove(0);
        capturer.start(&target).unwrap();
        let frame = capturer.next_frame().unwrap();
        let FramePixels::Rgba16F { data, stride } = frame.pixels else {
            panic!("se esperaba RGBA de coma flotante");
        };
        assert_eq!(stride, 64 * 8);
        assert_eq!(data.len(), 64 * 48 * 8);
        let side = 48 / 6;
        let (bx, by) = capturer.block_position(0, side);
        let at = (by * 64 + bx) * 8;
        let red = u16::from_le_bytes([data[at], data[at + 1]]);
        assert_eq!(red, half_bits(HDR_BLOCK_SCRGB), "bloque a 1000 nits");
        assert_eq!(half_bits(1.0), 0x3c00);
        assert_eq!(half_bits(12.5), 0x4a40);
        assert_eq!(hdr_table()[0], 0);
        // El blanco SDR va al blanco de referencia (203 nits = 2,5375 en scRGB ≈ 0x4113).
        assert_eq!(hdr_table()[255], half_bits(203.0 / 80.0));
    }

    #[test]
    fn has_a_single_synthetic_target() {
        let capturer = SyntheticCapturer::new(320, 180, 60);
        assert_eq!(
            capturer.targets().unwrap(),
            vec![CaptureTarget::Monitor {
                id: 0,
                name: "Sintético".into()
            }]
        );
    }

    #[test]
    fn needs_start_before_frames() {
        let mut capturer = SyntheticCapturer::new(64, 64, 60);
        assert!(capturer.next_frame().is_err());
    }

    #[test]
    fn frames_have_the_requested_size_and_cadence() {
        let mut capturer = SyntheticCapturer::new(320, 180, 100);
        let target = capturer.targets().unwrap().remove(0);
        capturer.start(&target).unwrap();
        let first = capturer.next_frame().unwrap();
        let mut last = first.captured_at;
        for id in 1..10 {
            let frame = capturer.next_frame().unwrap();
            assert_eq!(frame.frame_id, id);
            assert_eq!((frame.width, frame.height), (320, 180));
            let FramePixels::Bgra { data, stride } = &frame.pixels else {
                panic!("se esperaba BGRA en CPU");
            };
            assert_eq!(*stride, 320 * 4);
            assert_eq!(data.len(), 320 * 4 * 180);
            let gap = frame.captured_at - last;
            assert!(
                gap >= Duration::from_millis(9),
                "cadencia de 10 ms, hubo {gap:?}"
            );
            last = frame.captured_at;
            assert!(frame.captured_at <= Instant::now());
        }
        capturer.stop();
        assert!(capturer.next_frame().is_err());
    }

    #[test]
    fn the_whole_frame_moves() {
        let capturer = SyntheticCapturer::new(256, 128, 60);
        let (a, _) = capturer.render(10);
        let (b, _) = capturer.render(11);
        // Cada fila del fondo cambia de un fotograma al siguiente.
        let stride = 256 * 4;
        let changed_rows = a
            .chunks_exact(stride)
            .zip(b.chunks_exact(stride))
            .filter(|(x, y)| x != y)
            .count();
        assert_eq!(changed_rows, 128);
    }

    #[test]
    fn counter_bars_encode_the_frame_id() {
        let capturer = SyntheticCapturer::new(640, 360, 60);
        let id = 0b1011u64;
        let (data, cursor) = capturer.render(id);
        let bar_w = 640 / 64;
        let bit = |b: usize| pixel(&data, 640, b * bar_w, 2) == [255, 255, 255, 255];
        let read: u64 = (0..32).fold(0, |acc, b| (acc << 1) | u64::from(bit(b)));
        assert_eq!(read, id);
        assert!(cursor.visible);
        assert!((0..640).contains(&cursor.x) && (0..360).contains(&cursor.y));
        let (_, next) = capturer.render(id + 1);
        assert_ne!((cursor.x, cursor.y), (next.x, next.y), "el cursor se mueve");
        assert_eq!(cursor.shape_id, CURSOR_SHAPE_ID);
    }

    #[test]
    fn frames_carry_a_small_fixed_cursor_shape() {
        let shape = cursor_shape();
        assert!(shape.is_valid());
        assert_eq!((shape.hotspot_x, shape.hotspot_y), (0, 0));
        // Punta opaca, esquina superior derecha transparente, interior blanco.
        assert_eq!(&shape.rgba[..4], &[0, 0, 0, 255]);
        assert_eq!(shape.rgba[(CURSOR_SIDE as usize - 1) * 4 + 3], 0);
        let inner = (6 * CURSOR_SIDE as usize + 3) * 4;
        assert_eq!(&shape.rgba[inner..inner + 4], &[255, 255, 255, 255]);
        let mut capturer = SyntheticCapturer::new(64, 64, 100);
        let target = capturer.targets().unwrap().remove(0);
        capturer.start(&target).unwrap();
        let a = capturer.next_frame().unwrap();
        let b = capturer.next_frame().unwrap();
        let (sa, sb) = (a.cursor_shape.unwrap(), b.cursor_shape.unwrap());
        assert!(
            Arc::ptr_eq(&sa, &sb),
            "la misma forma en todos los fotogramas"
        );
        assert_eq!(a.cursor.unwrap().shape_id, sa.shape_id);
    }
}
