#!/usr/bin/env bash
# Descarga a third_party/meson-packagecache/ los paquetes de meson que necesita la compilación de
# `webrtc-audio-processing` (procesado de voz en Linux): abseil-cpp 20240722.0 y su parche
# de WrapDB, los mismos ficheros y SHA-256 que fija `subprojects/abseil-cpp.wrap` de la crate.
# `.cargo/config.toml` apunta `MESON_PACKAGE_CACHE_DIR` a ese directorio, así meson no descarga
# nada al compilar (wrapdb.mesonbuild.com puede no estar alcanzable desde la CI o un proxy).
#
#   tools/fetch-meson-wraps.sh            descarga y comprueba lo que falte
#   tools/fetch-meson-wraps.sh --force    lo vuelve a descargar
#
# Necesita curl y sha256sum.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEST="$ROOT/third_party/meson-packagecache"
FORCE=0

die() {
    echo "error: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
    --force) FORCE=1 ;;
    -h | --help)
        sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *) die "opción desconocida: $1 (prueba --help)" ;;
    esac
    shift
done

for tool in curl sha256sum; do
    command -v "$tool" >/dev/null || die "falta \`$tool\`"
done

# nombre|SHA-256|URL (la de WrapDB, con su copia en las releases de GitHub)
FILES=(
    "abseil-cpp-20240722.0.tar.gz|f50e5ac311a81382da7fa75b97310e4b9006474f9560ac46f54a9967f07d4ae3|https://github.com/abseil/abseil-cpp/releases/download/20240722.0/abseil-cpp-20240722.0.tar.gz"
    "abseil-cpp_20240722.0-3_patch.zip|12dd8df1488a314c53e3751abd2750cf233b830651d168b6a9f15e7d0cf71f7b|https://github.com/mesonbuild/wrapdb/releases/download/abseil-cpp_20240722.0-3/abseil-cpp_20240722.0-3_patch.zip"
)

mkdir -p "$DEST"
for entry in "${FILES[@]}"; do
    IFS='|' read -r name sum url <<<"$entry"
    file="$DEST/$name"
    if [ "$FORCE" -eq 0 ] && [ -f "$file" ] && echo "$sum  $file" | sha256sum -c --quiet - 2>/dev/null; then
        echo "$name: ya está"
        continue
    fi
    echo "Descargando $name…"
    curl -fsSL --retry 3 -o "$file.part" "$url" || die "no se pudo descargar $url"
    echo "$sum  $file.part" | sha256sum -c --quiet - || die "$name no tiene el SHA-256 esperado"
    mv "$file.part" "$file"
done
echo "Paquetes de meson en $DEST"
