#!/usr/bin/env bash
# Descarga la build de FFmpeg fijada en tools/ffmpeg.lock (BtbN, LGPL compartida) y la deja en
# third_party/ffmpeg/, que es donde la busca la compilación (.cargo/config.toml).
#
#   tools/fetch-ffmpeg.sh                 descarga y comprueba la build de Linux del lock
#   tools/fetch-ffmpeg.sh --force         la vuelve a descargar aunque ya esté
#   tools/fetch-ffmpeg.sh --dest DIR      la extrae en DIR en lugar de third_party/ffmpeg
#   tools/fetch-ffmpeg.sh --pin [TAG]     fija en el lock el release TAG (por defecto, el autobuild
#                                         más reciente) con los nombres y SHA-256 de Linux y Windows
#
# Necesita curl, tar con xz y sha256sum; --pin además python3 (y GITHUB_TOKEN, si lo hay, para no
# chocar con el límite de la API de GitHub).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
LOCK="$ROOT/tools/ffmpeg.lock"
DEST="$ROOT/third_party/ffmpeg"
REPO="BtbN/FFmpeg-Builds"
FORCE=0
PIN=0
PIN_TAG=""

die() {
    echo "error: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
    --force) FORCE=1 ;;
    --dest)
        shift
        [ $# -gt 0 ] || die "--dest necesita un directorio"
        DEST=$1
        ;;
    --pin)
        PIN=1
        if [ $# -gt 1 ] && [ "${2#--}" = "$2" ]; then
            PIN_TAG=$2
            shift
        fi
        ;;
    -h | --help)
        sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *) die "opción desconocida: $1 (prueba --help)" ;;
    esac
    shift
done

# Lee CLAVE=valor del lock sin ejecutarlo.
lock_value() {
    sed -n "s/^$1=//p" "$LOCK" | head -n 1 | tr -d '\r'
}

for tool in curl tar sha256sum; do
    command -v "$tool" >/dev/null || die "falta \`$tool\`"
done

VERSION=$(lock_value FFMPEG_VERSION)
[ -n "$VERSION" ] || die "tools/ffmpeg.lock no tiene FFMPEG_VERSION"

api() {
    local auth=()
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
    fi
    curl -fsSL "${auth[@]}" -H "Accept: application/vnd.github+json" "https://api.github.com/repos/$REPO/$1"
}

if [ "$PIN" = 1 ]; then
    command -v python3 >/dev/null || die "--pin necesita python3"
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    if [ -n "$PIN_TAG" ]; then
        [ "$PIN_TAG" != latest ] || die "no se fija \`latest\`: su contenido cambia cada día"
        api "releases/tags/$PIN_TAG" >"$WORK/release.json" || die "no existe el release $PIN_TAG"
    else
        api "releases?per_page=30" >"$WORK/releases.json" || die "no se pudo listar los releases de $REPO"
        python3 -I - "$WORK/releases.json" "$WORK/release.json" <<'EOF'
import json, sys
releases = json.load(open(sys.argv[1]))
dated = [r for r in releases if r.get("tag_name", "").startswith("autobuild-")]
if not dated:
    sys.exit("no hay ningún release autobuild-*")
json.dump(dated[0], open(sys.argv[2], "w"))
EOF
    fi
    # Nombres de los dos ficheros y URL de checksums.sha256 si existe.
    python3 -I - "$WORK/release.json" "$VERSION" >"$WORK/pin.txt" <<'EOF'
import json, sys
release, version = json.load(open(sys.argv[1])), sys.argv[2]
names = [a["name"] for a in release.get("assets", [])]
def pick(platform, ext):
    tail = f"-{platform}-lgpl-shared-{version}.{ext}"
    found = [n for n in names if n.startswith(f"ffmpeg-n{version}") and n.endswith(tail)]
    if len(found) != 1:
        sys.exit(f"el release {release['tag_name']} no tiene un único *{tail}: {found}")
    return found[0]
print(release["tag_name"])
print(pick("linux64", "tar.xz"))
print(pick("win64", "zip"))
print("checksums.sha256" if "checksums.sha256" in names else "")
EOF
    { read -r TAG; read -r LINUX_FILE; read -r WINDOWS_FILE; read -r SUMS; } <"$WORK/pin.txt"
    BASE="https://github.com/$REPO/releases/download/$TAG"
    sha_of() {
        local name=$1 sum=""
        if [ -n "$SUMS" ]; then
            [ -f "$WORK/sums" ] || curl -fsSL -o "$WORK/sums" "$BASE/$SUMS"
            sum=$(awk -v n="$name" '{ f = $2; sub(/^\*/, "", f) } f == n { print $1 }' "$WORK/sums")
        fi
        if [ -z "$sum" ]; then
            echo "Calculando el SHA-256 de $name (el release no lo publica)…" >&2
            curl -fL --progress-bar -o "$WORK/$name" "$BASE/$name"
            sum=$(sha256sum "$WORK/$name" | cut -d' ' -f1)
            rm -f "$WORK/$name"
        fi
        echo "$sum"
    }
    LINUX_SHA=$(sha_of "$LINUX_FILE")
    WINDOWS_SHA=$(sha_of "$WINDOWS_FILE")
    python3 -I - "$LOCK" "$TAG" "$LINUX_FILE" "$LINUX_SHA" "$WINDOWS_FILE" "$WINDOWS_SHA" <<'EOF'
import sys
path, tag, lf, ls, wf, ws = sys.argv[1:]
values = {"BTBN_TAG": tag, "LINUX_FILE": lf, "LINUX_SHA256": ls, "WINDOWS_FILE": wf, "WINDOWS_SHA256": ws}
out = []
for line in open(path, encoding="utf-8"):
    key = line.split("=", 1)[0]
    out.append(f"{key}={values[key]}\n" if "=" in line and key in values else line)
open(path, "w", encoding="utf-8").write("".join(out))
EOF
    echo "tools/ffmpeg.lock fijado en $TAG:"
    echo "  $LINUX_FILE  $LINUX_SHA"
    echo "  $WINDOWS_FILE  $WINDOWS_SHA"
    echo "Quita la nota «ESTADO: sin fijar» del lock si sigue ahí."
    exit 0
fi

TAG=$(lock_value BTBN_TAG)
FILE=$(lock_value LINUX_FILE)
SHA=$(lock_value LINUX_SHA256)
if [ -z "$TAG" ] || [ -z "$FILE" ] || [ -z "$SHA" ]; then
    die "tools/ffmpeg.lock no tiene fijada la build de Linux (etiqueta, fichero y SHA-256).
Fíjala desde una máquina con acceso a github.com con \`tools/fetch-ffmpeg.sh --pin\`, o usa otra
build de FFmpeg ≥ 8.0 con FFMPEG_INCLUDE_DIR y FFMPEG_LIBS_DIR (ver README.md, «FFmpeg»)."
fi

STAMP="$DEST/.coroom-ffmpeg"
WANT="$TAG $FILE $SHA"
if [ "$FORCE" = 0 ] && [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$WANT" ]; then
    echo "FFmpeg $VERSION ($TAG) ya está en $DEST."
else
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    URL="https://github.com/$REPO/releases/download/$TAG/$FILE"
    echo "Descargando $FILE ($TAG)…"
    curl -fL --retry 3 --progress-bar -o "$WORK/$FILE" "$URL" || die "no se pudo descargar $URL"
    echo "$SHA  $WORK/$FILE" | sha256sum -c --quiet - ||
        die "el SHA-256 de $FILE no coincide con tools/ffmpeg.lock"
    mkdir -p "$WORK/x"
    tar -xJf "$WORK/$FILE" -C "$WORK/x" --strip-components=1
    [ -f "$WORK/x/include/libavcodec/avcodec.h" ] || die "$FILE no trae include/libavcodec"
    rm -rf "$DEST"
    mkdir -p "$(dirname "$DEST")"
    mv "$WORK/x" "$DEST"
    echo "$WANT" >"$STAMP"
    echo "FFmpeg $VERSION extraído en $DEST."
fi

FFMPEG="$DEST/bin/ffmpeg"
if [ -x "$FFMPEG" ]; then
    export LD_LIBRARY_PATH="$DEST/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    "$FFMPEG" -hide_banner -version | head -n 1
    echo "Codificadores de interés:"
    "$FFMPEG" -hide_banner -encoders | grep -E "vulkan|nvenc|amf|qsv|vaapi|svtav1" || echo "  (ninguno)"
    echo "Decodificadores de interés:"
    "$FFMPEG" -hide_banner -decoders | grep -E "dav1d|cuvid|qsv" || echo "  (ninguno)"
fi
