#!/usr/bin/env bash
# Repositorio Flatpak de coroom en GitHub Pages y el `.flatpak` de la publicación.
#
# Uso: tools/flatpak-repo.sh REPO_DE_BUILD PAQUETE.flatpak DIRECTORIO_DEL_SITIO
#
# REPO_DE_BUILD es el repositorio OSTree donde flatpak-builder dejó la build (rama `stable`).
#
# Con la clave en COROOM_FLATPAK_GPG_KEY (OpenPGP en ASCII, sin contraseña; la CI solo la pasa en
# las publicaciones de main y en la prueba a mano de flatpak.yml):
#
# 1. Copia el repositorio que hay publicado en Pages (`ostree pull --mirror`, las últimas
#    versiones): una publicación de Pages sustituye el sitio entero, así que lo publicado se parte
#    de ahí. La primera vez no hay nada (404) y se empieza de cero.
# 2. Pone la build encima como commit nuevo de `app/<id>/x86_64/stable` (su padre es la versión
#    publicada), firmado.
# 3. `flatpak build-update-repo`: resumen firmado, deltas estáticas desde la versión anterior (las
#    actualizaciones bajan solo lo que cambió) y poda a `prune_depth` versiones anteriores.
# 4. El `.flatpak` sale de ese commit con la URL del repositorio y la clave pública: al instalarlo,
#    Flatpak configura el repositorio y desde ahí se actualiza solo.
# 5. Deja en DIRECTORIO_DEL_SITIO el sitio entero: `repo/`, `coroom.flatpakrepo`,
#    `<id>.flatpakref`, la clave pública, el icono, `version` e `index.html`, y escribe
#    `publish=true` en $GITHUB_OUTPUT para que ci.yml lo suba a Pages.
#
# Nada de esto puede dejar la publicación sin paquetes: si cualquier paso de la parte firmada falla
# (la copia, la firma, las deltas, la comprobación, una herramienta que falta), se avisa y el
# `.flatpak` sale sin firmar y sin repositorio, como sin clave, con `publish=false`.
#
# Entorno: VERSION (X.Y.Z, para la página), GITHUB_REPOSITORY (propietario/nombre: la URL de Pages
# y la página del proyecto), COROOM_PAGES_URL (otra URL del sitio, para probar en local),
# COROOM_FLATPAK_RELEASE=true (es una publicación: sin clave, avisa).
set -euo pipefail

signed=false
if [ "${1:-}" = --signed ]; then
    signed=true
    shift
fi
if [ $# -ne 3 ]; then
    echo "uso: $0 REPO_DE_BUILD PAQUETE.flatpak DIRECTORIO_DEL_SITIO" >&2
    exit 2
fi
build_repo="$1"
bundle="$2"
site="$3"
root="$(cd "$(dirname "$0")/.." && pwd)"

app_id=io.github._43fdfdg45454.coroom
arch=x86_64
branch=stable
runtime_repo=https://dl.flathub.org/repo/flathub.flatpakrepo
# Versiones anteriores que se guardan además de la última (y sus deltas): las actualizaciones desde
# ellas bajan solo lo que cambió; desde más atrás, los objetos que cambiaron, sin delta.
prune_depth=2
# Aviso de caducidad de la clave, con tiempo para renovarla.
expiry_warning_days=90
version="${VERSION:-1.0.0}"
github_repository="${GITHUB_REPOSITORY:-43fdfdg45454/coroom}"
owner="${github_repository%%/*}"
name="${github_repository#*/}"
pages_url="${COROOM_PAGES_URL:-https://${owner,,}.github.io/$name/}"
pages_url="${pages_url%/}/"
repo_url="${pages_url}repo/"
homepage="https://github.com/$github_repository"

notice() {
    if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::$1::$2"; else echo "$1: $2" >&2; fi
}

output() {
    if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "$1" >> "$GITHUB_OUTPUT"; fi
}

unsigned_bundle() {
    rm -f "$bundle"
    flatpak build-bundle --arch="$arch" --runtime-repo="$runtime_repo" \
        "$build_repo" "$bundle" "$app_id" "$branch"
    ls -l "$bundle"
}

# --- Sin clave, o la parte firmada falló: el paquete sin firmar --------------------------------
if [ "$signed" = false ]; then
    if [ -z "${COROOM_FLATPAK_GPG_KEY:-}" ]; then
        if [ "${COROOM_FLATPAK_RELEASE:-false}" = true ]; then
            notice warning "sin COROOM_FLATPAK_GPG_KEY: el .flatpak va sin firmar y sin repositorio, y GitHub Pages no se actualiza (las instalaciones no se actualizan solas)"
        else
            notice notice "no es una publicación de main: el .flatpak va sin firmar y sin repositorio, y no se publica nada"
        fi
        unsigned_bundle
        output publish=false
        exit 0
    fi
    # La parte firmada va en otro proceso: así `set -e` corta en el primer fallo y aquí se decide.
    if bash "${BASH_SOURCE[0]}" --signed "$build_repo" "$bundle" "$site"; then
        output publish=true
        exit 0
    fi
    notice warning "no se pudo firmar ni preparar el repositorio Flatpak (mira los mensajes de arriba): el .flatpak de esta versión va sin firmar y sin repositorio, y GitHub Pages no se actualiza"
    rm -rf "$site"
    unsigned_bundle
    output publish=false
    exit 0
fi

# --- La parte firmada (proceso hijo: cualquier error sale con estado distinto de cero) ---------
fail() {
    notice "${2:-warning}" "$1"
    exit 1
}

for tool in gpg gpgconf ostree flatpak curl base64; do
    command -v "$tool" >/dev/null || fail "falta $tool para firmar el repositorio Flatpak"
done

# La clave privada vive solo en un GNUPGHOME temporal, que se borra al salir.
GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
chmod 700 "$GNUPGHOME"
work="$(mktemp -d)"
cleanup() {
    gpgconf --kill gpg-agent 2>/dev/null || true
    rm -rf "$GNUPGHOME" "$work"
}
trap cleanup EXIT

printf '%s\n' "$COROOM_FLATPAK_GPG_KEY" | gpg --batch --quiet --import ||
    fail "COROOM_FLATPAK_GPG_KEY no es una clave OpenPGP en ASCII"
keys="$(gpg --batch --with-colons --list-secret-keys)"
fingerprint="$(awk -F: '$1 == "fpr" { print $10; exit }' <<<"$keys")"
[ -n "$fingerprint" ] || fail "COROOM_FLATPAK_GPG_KEY no trae una clave privada OpenPGP"
echo "Firma con la clave $fingerprint"

# La clave pública se publica con sus identidades: ningún correo real (regla de privacidad), solo
# los de ejemplo.
while IFS= read -r uid; do
    email="$(sed -n 's/.*<\([^>]*\)>.*/\1/p' <<<"$uid")"
    if [ -n "$email" ] && [[ "${email,,}" != *@example.com ]]; then
        fail "la clave del repositorio Flatpak lleva un correo en su identidad y se publicaría en GitHub Pages: crea una sin correo (como «coroom (repositorio Flatpak)») o con uno @example.com" error
    fi
done < <(awk -F: '$1 == "uid" { print $10 }' <<<"$keys")

# Caducidad: de la clave principal y de la subclave que firma, la primera que caduque.
now="$(date +%s)"
expires="$(awk -F: '($1 == "sec" || ($1 == "ssb" && $12 ~ /s/)) && $7 != "" { print $7 }' <<<"$keys" | sort -n | head -1)"
if [ -n "$expires" ]; then
    days=$(( (expires - now) / 86400 ))
    if [ "$expires" -le "$now" ]; then
        fail "la clave del repositorio Flatpak caducó: renuévala (gpg --quick-set-expire $fingerprint) y actualiza COROOM_FLATPAK_GPG_KEY"
    elif [ "$days" -lt "$expiry_warning_days" ]; then
        notice warning "la clave del repositorio Flatpak caduca en $days días: renuévala (gpg --quick-set-expire $fingerprint) y actualiza COROOM_FLATPAK_GPG_KEY; si caduca, las instalaciones no aceptan las actualizaciones"
    fi
fi

rm -rf "$site"
mkdir -p "$site"
site="$(cd "$site" && pwd)"
repo="$site/repo"
public_key="$site/coroom.gpg"
gpg --batch --export --export-options export-minimal "$fingerprint" > "$public_key"

# 1. Lo publicado: la última versión y `prune_depth` anteriores, con sus firmas comprobadas con la
#    clave de ahora. Si no se puede copiar (después de un segundo intento), se empieza de cero: las
#    instalaciones se actualizan igual, con la versión nueva entera, sin delta.
fresh_repo() {
    rm -rf "$repo"
    ostree init --repo="$repo" --mode=archive
}
mirror() {
    fresh_repo
    ostree remote add --repo="$repo" --gpg-import="$public_key" \
        --set=gpg-verify=true --set=gpg-verify-summary=true published "$repo_url" &&
        ostree pull --repo="$repo" --mirror --depth="$prune_depth" published &&
        ostree remote delete --repo="$repo" published
}
fresh_repo
status="$(curl -sSL -o /dev/null -w '%{http_code}' "${repo_url}config" || true)"
case "$status" in
    200)
        echo "Copia del repositorio publicado en $repo_url"
        if ! mirror; then
            echo "Segundo intento de copiar el repositorio publicado"
            if ! mirror; then
                notice warning "no se pudo copiar el repositorio publicado en $repo_url: se empieza de cero (esta versión se baja entera, sin delta)"
                fresh_repo
            fi
        fi
        ;;
    404)
        echo "Todavía no hay repositorio publicado en $repo_url: se empieza de cero"
        ;;
    *)
        notice warning "el repositorio publicado en $repo_url responde «$status»: se empieza de cero (esta versión se baja entera, sin delta)"
        ;;
esac
rm -f "$repo"/published.trustedkeys.gpg

# 2. La build encima, firmada: la aplicación y, si la hay, su extensión de idiomas. La de depuración
#    (`.Debug`) no se publica (no se instala sola y es lo que más pesa).
build_refs="$(ostree refs --repo="$build_repo")"
refs="$(grep -E "^(app/$app_id|runtime/$app_id\.Locale)/$arch/$branch\$" <<<"$build_refs" || true)"
grep -qx "app/$app_id/$arch/$branch" <<<"$refs" || fail "$build_repo no tiene app/$app_id/$arch/$branch"
for ref in $refs; do
    flatpak build-commit-from --src-repo="$build_repo" --no-update-summary \
        --gpg-sign="$fingerprint" --gpg-homedir="$GNUPGHOME" "$repo" "$ref"
done

# 3. Resumen firmado (con la clave pública dentro, por si un día cambia), deltas y poda.
flatpak build-update-repo --gpg-sign="$fingerprint" --gpg-homedir="$GNUPGHOME" \
    --gpg-import="$public_key" --title=coroom --comment="Juega en remoto con tus amigos" \
    --homepage="$homepage" --icon="${pages_url}coroom.svg" --default-branch="$branch" \
    --generate-static-deltas --prune --prune-depth="$prune_depth" "$repo"
find "$repo/tmp" -mindepth 1 -delete

# 4. El paquete de la publicación, del commit firmado y con el repositorio configurado.
rm -f "$bundle"
flatpak build-bundle --arch="$arch" --runtime-repo="$runtime_repo" \
    --repo-url="$repo_url" --gpg-keys="$public_key" "$repo" "$bundle" "$app_id" "$branch"

# Lo que hará un cliente: el resumen y el commit se comprueban con la clave pública (y el paquete
# lleva ese mismo commit firmado, que Flatpak exige para instalarlo con la clave).
check="$work/check"
ostree init --repo="$check" --mode=bare-user
ostree remote add --repo="$check" --gpg-import="$public_key" \
    --set=gpg-verify=true --set=gpg-verify-summary=true site "file://$repo"
ostree pull --repo="$check" --commit-metadata-only site "app/$app_id/$arch/$branch"
shown="$(ostree show --repo="$check" --gpg-verify-remote=site "app/$app_id/$arch/$branch")"
grep -q "Good signature" <<<"$shown" || fail "el commit publicado no tiene una firma válida de $fingerprint"

# 5. Ficheros para instalar desde el sitio, la versión publicada (ci.yml no despliega encima una
#    más vieja) y la página que lo explica.
gpg_key_base64="$(base64 -w0 < "$public_key")"
cat > "$site/coroom.flatpakrepo" <<EOF
[Flatpak Repo]
Title=coroom
Url=$repo_url
Homepage=$homepage
Comment=Juega en remoto con tus amigos
Description=Repositorio Flatpak de coroom, firmado. Se actualiza con cada versión publicada.
Icon=${pages_url}coroom.svg
DefaultBranch=$branch
GPGKey=$gpg_key_base64
EOF
cat > "$site/$app_id.flatpakref" <<EOF
[Flatpak Ref]
Name=$app_id
Branch=$branch
Title=coroom
Url=$repo_url
SuggestRemoteName=coroom
Homepage=$homepage
Icon=${pages_url}coroom.svg
RuntimeRepo=$runtime_repo
IsRuntime=false
GPGKey=$gpg_key_base64
EOF
echo "$version" > "$site/version"
cp "$root/packaging/icons/coroom.svg" "$site/coroom.svg"
sed -e "s|@PAGES_URL@|$pages_url|g" -e "s|@VERSION@|$version|g" -e "s|@APP_ID@|$app_id|g" \
    -e "s|@HOMEPAGE@|$homepage|g" -e "s|@FINGERPRINT@|$fingerprint|g" \
    "$root/packaging/flatpak/pages/index.html" > "$site/index.html"

history="$(ostree log --repo="$repo" "app/$app_id/$arch/$branch")"
grep -E '^(commit|Date)' <<<"$history" || true
deltas="$(find "$repo/deltas" -name superblock 2>/dev/null | wc -l)"
size="$(du -sh "$site" | cut -f1)"
echo "Sitio: $size, $deltas deltas"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "Repositorio Flatpak $version: $size, $deltas deltas, firmado con $fingerprint" >> "$GITHUB_STEP_SUMMARY"
fi
ls -l "$bundle"
