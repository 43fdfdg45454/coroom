#!/usr/bin/env python3
"""Borra de un directorio de perfil de cargo (target/release, target/<triple>/release) lo que la
última compilación no usó: versiones viejas de dependencias y de las crates del workspace, que
cargo nunca borra. Así el `target/` que guarda la CI entre ejecuciones (Flatpak e imagen del
servidor) no crece con cada commit.

Uso: prune-cargo-target.py DIR_DEL_PERFIL MENSAJES.json

MENSAJES.json es la salida de `cargo build --message-format=json-render-diagnostics`: trae un
`compiler-artifact` por unidad, también de las que ya estaban al día, y un `build-script-executed`
por script de compilación. Se conserva todo lo que lleve el hash de alguna de esas unidades; en
`deps/` y `build/` se borra lo demás. Los ejecutables son la excepción: cargo los anuncia por su
copia sin hash (target/release/NOMBRE), así que de `deps/NOMBRE-<hash>` se conserva el más reciente,
que es el de la última compilación que lo enlazó. `.fingerprint/` no se toca (son ficheros pequeños, y si una
huella apunta a un artefacto borrado, cargo lo recompila).
"""

import json
import re
import shutil
import sys
from pathlib import Path

HASH = re.compile(r"-([0-9a-f]{16})(?=[./]|$)")


def used_units(messages: Path) -> tuple[set[str], set[str]]:
    """Hashes de las unidades usadas y nombres (con `_`) de los ejecutables."""
    hashes, binaries = set(), set()
    for line in messages.read_text(encoding="utf-8").splitlines():
        if not line.startswith("{"):
            continue
        msg = json.loads(line)
        paths = []
        if msg.get("reason") == "compiler-artifact":
            paths = msg.get("filenames", [])
            if msg.get("executable"):
                binaries.add(msg["target"]["name"].replace("-", "_"))
        elif msg.get("reason") == "build-script-executed":
            paths = [msg.get("out_dir", "")]
        for path in paths:
            for part in Path(path).parts:
                hashes.update(HASH.findall(part))
    return hashes, binaries


def newest_binaries(deps: Path, binaries: set[str]) -> set[str]:
    """El hash del `deps/NOMBRE-<hash>` más reciente de cada ejecutable."""
    newest = {}
    for entry in deps.glob("*"):
        # Solo el ejecutable (sin extensión): su `.d` va con el mismo hash, y una biblioteca del
        # mismo nombre deja otro `NOMBRE-<hash>.d` que no cuenta.
        found = HASH.findall(entry.name)
        name = entry.name.rsplit("-", 1)[0]
        if not found or "." in entry.name or name not in binaries:
            continue
        mtime = entry.lstat().st_mtime
        if name not in newest or mtime > newest[name][0]:
            newest[name] = (mtime, found[-1])
    return {h for _, h in newest.values()}


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    profile, messages = Path(sys.argv[1]), Path(sys.argv[2])
    keep, binaries = used_units(messages)
    if not keep and not binaries:
        # Sin mensajes (cargo falló o se llamó mal) no se sabe qué se usa: no se borra nada.
        print("prune-cargo-target: sin artefactos en los mensajes; no se borra nada", file=sys.stderr)
        return 1
    keep |= newest_binaries(profile / "deps", binaries)
    removed = freed = 0
    for sub in ("deps", "build"):
        for entry in (profile / sub).glob("*"):
            found = HASH.findall(entry.name)
            if not found or found[-1] in keep:
                continue
            if entry.is_dir() and not entry.is_symlink():
                freed += sum(f.stat().st_size for f in entry.rglob("*") if f.is_file())
                shutil.rmtree(entry)
            else:
                freed += entry.lstat().st_size
                entry.unlink()
            removed += 1
            print(f"prune-cargo-target: {sub}/{entry.name}")
    print(f"prune-cargo-target: {removed} entradas viejas borradas ({freed / 2**20:.0f} MiB)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
