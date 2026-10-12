// Conversión Y′CbCr → RGB para el vídeo decodificado (planar: I420 o I444 en tres planos;
// semiplanar: NV12 o P010 en dos), con bandas negras y el cursor local: la forma real (textura
// RGBA con su punto activo) o, mientras no llega, un cuadrado blanco.
//
// Matriz: la afín de `crate::color::yuv_to_rgb` (BT.709 en rango limitado como
// `coroom_codec::convert`, o BT.2020 a 10 bits), con el rango y la profundidad ya dentro.
// Transferencia: SDR tal cual; PQ (HDR10) → nits → BT.709 relativo al blanco de 203 nits →
// recorte suave al margen de la pantalla y sRGB extendido en una salida HDR, o tone mapping
// (rodilla en 0,9 y hombro de Reinhard sobre el máximo de RGB) y sRGB en una SDR. Las mismas
// cuentas, en Rust, en `crate::color::output`.
//
// La salida está en espacio gamma (como el BGRA capturado): el presentador escribe en una vista
// sin sufijo sRGB para que no se codifique dos veces (en HDR, `crate::hdr` la pasa a lineal).

struct Params {
    // Rectángulo del vídeo en píxeles del destino: origen (x, y) y tamaño (w, h).
    video_rect: vec4<f32>,
    // Cursor de respaldo: centro en píxeles del destino (x, y), semilado (z) y visibilidad (w).
    cursor: vec4<f32>,
    // Cursor con forma: origen (x, y) y tamaño (w, h) en píxeles del destino.
    cursor_rect: vec4<f32>,
    // x: 1 si el fotograma es semiplanar (croma en `tex_uv`); y: 0 sin cursor, 1 cuadrado, 2 con
    // forma; z, w: parte visible de la textura (1 salvo en las imágenes importadas del
    // decodificador, que pueden ser más grandes que el fotograma).
    mode: vec4<f32>,
    // Matriz afín Y′CbCr → R′G′B′, por filas.
    yuv_r: vec4<f32>,
    yuv_g: vec4<f32>,
    yuv_b: vec4<f32>,
    // x: 1 con PQ; y: 1 con salida HDR; z: margen de la pantalla (múltiplos del blanco SDR).
    color: vec4<f32>,
};

@group(0) @binding(0) var tex_y: texture_2d<f32>;
@group(0) @binding(1) var tex_u: texture_2d<f32>;
@group(0) @binding(2) var tex_v: texture_2d<f32>;
@group(0) @binding(3) var video_sampler: sampler;
@group(0) @binding(4) var<uniform> params: Params;
@group(0) @binding(5) var tex_uv: texture_2d<f32>;
@group(0) @binding(6) var tex_cursor: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

// Triángulo que cubre toda la pantalla, sin búfer de vértices.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let x = f32(i32(index & 1u) * 4 - 1);
    let y = f32(i32(index & 2u) * 2 - 1);
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    return out;
}

fn yuv_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    let sample = vec4<f32>(y, u, v, 1.0);
    return vec3<f32>(dot(params.yuv_r, sample), dot(params.yuv_g, sample), dot(params.yuv_b, sample));
}

// EOTF de PQ (SMPTE ST 2084): señal en [0, 1] → nits.
fn pq_to_nits(signal: vec3<f32>) -> vec3<f32> {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 4096.0 * 128.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 4096.0 * 32.0;
    let c3 = 2392.0 / 4096.0 * 32.0;
    let e = pow(clamp(signal, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / m2));
    let y = pow(max(e - c1, vec3<f32>(0.0)) / (c2 - c3 * e), vec3<f32>(1.0 / m1));
    return y * 10000.0;
}

fn linear_to_srgb(v: vec3<f32>) -> vec3<f32> {
    let a = abs(v);
    let low = a * 12.92;
    let high = 1.055 * pow(a, vec3<f32>(1.0 / 2.4)) - 0.055;
    return sign(v) * select(high, low, a <= vec3<f32>(0.0031308));
}

// Rodilla en 0,9 de `peak` y hombro de Reinhard sobre el máximo de RGB (`crate::color::tone_map_peak`).
fn tone_map_peak(rgb: vec3<f32>, peak_in: f32) -> vec3<f32> {
    let peak = max(peak_in, 1.0);
    let c = max(rgb, vec3<f32>(0.0)) / peak;
    let top = max(c.r, max(c.g, c.b));
    let knee = 0.9;
    if (top <= knee) {
        return c * peak;
    }
    let t = (top - knee) / (1.0 - knee);
    let mapped = knee + (1.0 - knee) * t / (1.0 + t);
    return c * (mapped / top * peak);
}

fn transfer(rgb: vec3<f32>) -> vec3<f32> {
    if (params.color.x < 0.5) {
        return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let relative = pq_to_nits(rgb) / 203.0;
    // BT.2020 → BT.709 en luz lineal (columnas de la matriz de `crate::color::BT2020_TO_BT709`).
    let to709 = mat3x3<f32>(
        vec3<f32>(1.660491, -0.124550, -0.018151),
        vec3<f32>(-0.587641, 1.132900, -0.100579),
        vec3<f32>(-0.072850, -0.008349, 1.118730),
    );
    let linear = to709 * relative;
    if (params.color.y > 0.5) {
        return linear_to_srgb(tone_map_peak(linear, params.color.z));
    }
    return linear_to_srgb(min(tone_map_peak(linear, 1.0), vec3<f32>(1.0)));
}

fn video_colour(pixel: vec2<f32>) -> vec3<f32> {
    let at = (pixel - params.video_rect.xy) / params.video_rect.zw;
    if (any(at < vec2<f32>(0.0)) || any(at > vec2<f32>(1.0))) {
        // Bandas negras para conservar la relación de aspecto.
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    let crop = select(vec2<f32>(1.0), params.mode.zw, params.mode.zw > vec2<f32>(0.0));
    let uv = at * crop;
    let y = textureSampleLevel(tex_y, video_sampler, uv, 0.0).r;
    var chroma: vec2<f32>;
    if (params.mode.x > 0.5) {
        chroma = textureSampleLevel(tex_uv, video_sampler, uv, 0.0).rg;
    } else {
        chroma = vec2<f32>(
            textureSampleLevel(tex_u, video_sampler, uv, 0.0).r,
            textureSampleLevel(tex_v, video_sampler, uv, 0.0).r,
        );
    }
    return transfer(yuv_to_rgb(y, chroma.x, chroma.y));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let pixel = in.position.xy;

    if (params.mode.y > 0.5 && params.mode.y < 1.5) {
        let offset = abs(pixel - params.cursor.xy);
        if (offset.x <= params.cursor.z && offset.y <= params.cursor.z) {
            return vec4<f32>(1.0, 1.0, 1.0, 1.0);
        }
    }

    var colour = video_colour(pixel);
    if (params.mode.y > 1.5) {
        let at = (pixel - params.cursor_rect.xy) / params.cursor_rect.zw;
        if (all(at >= vec2<f32>(0.0)) && all(at < vec2<f32>(1.0))) {
            // RGBA sin premultiplicar, en espacio gamma como el vídeo.
            let cursor = textureSampleLevel(tex_cursor, video_sampler, at, 0.0);
            colour = mix(colour, cursor.rgb, cursor.a);
        }
    }
    return vec4<f32>(colour, 1.0);
}
