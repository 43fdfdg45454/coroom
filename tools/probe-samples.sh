#!/usr/bin/env bash
# Genera las muestras 4:4:4 del sondeo de decodificadores (crates/codec/src/ffmpeg/samples/):
# el patrón del sondeo (test `write_the_444_pattern`) codificado en HEVC Main 4:4:4 con x265 y en
# AV1 High (4:4:4) con libaom, sin fotogramas B, un fotograma por paquete. Cada muestra es una
# lista de fotogramas, cada uno con su longitud delante (u32 little-endian).
#
# Hace falta un `ffmpeg` con libx265 y libaom (el de la distribución sirve). Uso: tools/probe-samples.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/crates/codec/src/ffmpeg/samples"
side=256
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

COROOM_PATTERN_OUT="$work/pattern.yuv" cargo test -q -p coroom-codec --lib \
    ffmpeg::probe::tests::write_the_444_pattern -- --ignored --exact >/dev/null

input=(-hide_banner -loglevel error -y -f rawvideo -pix_fmt yuv444p -s "${side}x${side}" -r 30
    -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 -i "$work/pattern.yuv")

ffmpeg "${input[@]}" -c:v libx265 -preset ultrafast -tune zerolatency -crf 18 \
    -x265-params "log-level=error:bframes=0:keyint=600:aud=1:repeat-headers=1" \
    -f hevc "$work/hevc-444.hevc"
ffmpeg "${input[@]}" -c:v libaom-av1 -usage realtime -cpu-used 8 -lag-in-frames 0 -crf 20 \
    -g 600 -f obu "$work/av1-444.obu"

python3 - "$work" "$out" <<'EOF'
import struct, sys
work, out = sys.argv[1], sys.argv[2]

def frame(chunks):
    return b"".join(struct.pack("<I", len(c)) + c for c in chunks)

def hevc_units(data):
    # Unidades de acceso: cada una empieza con su delimitador (NAL 35, `aud=1`).
    aud = b"\x00\x00\x00\x01\x46\x01"
    starts = [i for i in range(len(data)) if data.startswith(aud, i)]
    return [data[a:b] for a, b in zip(starts, starts[1:] + [len(data)])]

def leb128(data, i):
    value, shift = 0, 0
    while True:
        byte = data[i]
        i += 1
        value |= (byte & 0x7F) << shift
        shift += 7
        if not byte & 0x80:
            return value, i

def av1_units(data):
    # Unidades temporales: cada una empieza con un OBU delimitador (tipo 2).
    units, i = [], 0
    while i < len(data):
        start, kind = i, (data[i] >> 3) & 0x0F
        has_extension, has_size = data[i] & 0x04, data[i] & 0x02
        i += 1 + (1 if has_extension else 0)
        assert has_size, "OBU sin tamaño"
        size, i = leb128(data, i)
        i += size
        if kind == 2:
            units.append(bytearray())
        units[-1] += data[start:i]
    return [bytes(u) for u in units]

for name, split in (("hevc-444", hevc_units), ("av1-444", av1_units)):
    ext = "hevc" if name.startswith("hevc") else "obu"
    with open(f"{work}/{name}.{ext}", "rb") as f:
        units = split(f.read())
    assert len(units) == 10, (name, len(units))
    with open(f"{out}/{name}.bin", "wb") as f:
        f.write(frame(units))
    print(name, len(units), "fotogramas")
EOF
