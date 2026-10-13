#!/usr/bin/env bash
# Comprueba que todo lo que se deriva de las dependencias está al día, para que no falle la CI:
#
# - `Cargo.lock` del workspace y el de `fuzz/` (workspace aparte: un cambio de dependencias en una
#   crate que usa el fuzzing deja su lock viejo, y la CI compila los objetivos con `--locked`);
# - `packaging/flatpak/cargo-sources.json` frente a `Cargo.lock` (el Flatpak compila sin red).
#
# Uso:
#   tools/check-locks.sh          # comprueba; sale con error y dice cómo arreglarlo
#   tools/check-locks.sh --fix    # actualiza los locks (lo mínimo: solo lo que falta)
#   tools/check-locks.sh --flatpak-only   # solo cargo-sources.json (sin cargo: el job del Flatpak)
#
# Lo llaman la CI (antes de compilar), el test `locks_are_up_to_date` (con `cargo test`), el gancho
# `.githooks/pre-push` (con `git config core.hooksPath .githooks`) y el de Claude Code antes de un
# `git push` (`.claude/settings.json`).
set -euo pipefail
cd "$(dirname "$0")/.."

fix=false
manifests=(Cargo.toml fuzz/Cargo.toml)
case "${1:-}" in
    --fix) fix=true ;;
    --flatpak-only) manifests=() ;;
    "") ;;
    *) echo "uso: $0 [--fix | --flatpak-only]" >&2; exit 2 ;;
esac
failed=0
for manifest in "${manifests[@]+"${manifests[@]}"}"; do
    lock="$(dirname "$manifest")/Cargo.lock"
    if cargo metadata --locked --format-version 1 --manifest-path "$manifest" >/dev/null 2>&1; then
        continue
    fi
    if $fix; then
        cargo update --workspace --manifest-path "$manifest"
        echo "actualizado: $lock"
    else
        echo "✗ $lock no está al día con $manifest (arréglalo con tools/check-locks.sh --fix)" >&2
        failed=1
    fi
done

python3 - <<'PY' || failed=1
import json, re, sys
lock = open("Cargo.lock", encoding="utf-8").read()
wanted = set()
for block in lock.split("[[package]]")[1:]:
    if 'source = "registry+' not in block:
        continue
    name = re.search(r'^name = "([^"]+)"', block, re.M).group(1)
    version = re.search(r'^version = "([^"]+)"', block, re.M).group(1)
    wanted.add(f"{name}-{version}")
sources = json.load(open("packaging/flatpak/cargo-sources.json", encoding="utf-8"))
have = {s["dest"].rsplit("/", 1)[-1] for s in sources if s.get("type") == "archive"}
missing, extra = sorted(wanted - have), sorted(have - wanted)
if missing or extra:
    print("✗ packaging/flatpak/cargo-sources.json no corresponde a Cargo.lock; regenéralo con "
          "flatpak-cargo-generator.py (ver el README)", file=sys.stderr)
    print("  faltan:", missing[:20], "sobran:", extra[:20], file=sys.stderr)
    sys.exit(1)
print(f"cargo-sources.json al día ({len(have)} crates vendorizadas para el Flatpak)")
PY

exit $failed
