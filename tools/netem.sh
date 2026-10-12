#!/usr/bin/env bash
# Aplica o quita `tc netem` en una interfaz (por defecto `lo`) para probar los medios con pérdida,
# jitter y cuello de botella. Solo Linux; necesita root (CAP_NET_ADMIN) y `iproute2`.
#
# Uso:
#   sudo tools/netem.sh apply              # 2 % y 10 ms ± 5 ms (la pérdida simulada de los tests)
#   sudo tools/netem.sh profile PERFIL     # un perfil de la matriz de red (abajo)
#   sudo tools/netem.sh clear              # lo quita
#   tools/netem.sh status                  # muestra la disciplina actual
#   tools/netem.sh profiles                # lista los perfiles
#
# Perfiles de la matriz de red (los mismos que `LossyConfig::profile`):
#   lan     0 % de pérdida, 1 ms
#   buena   0,5 %, 15 ms ± 3 ms
#   media   2 %, 30 ms ± 10 ms
#   mala    5 %, 60 ms ± 20 ms y 1 % de reordenación
#   cuello  10 ms y el enlace limitado a 8 Mbit/s (tbf con 200 ms de cola)
#
# Variables opcionales de `apply`: NETEM_DEV (lo), NETEM_LOSS (2%), NETEM_DELAY (10ms),
# NETEM_JITTER (5ms). En `lo` cada paquete pasa una vez por la cola, así que cada sentido recibe
# la pérdida y el retardo. Los tests `netem_*` detectan netem en `lo` y se saltan si no lo hay:
#   cargo test -p coroom-transport -- netem
#   cargo test -p coroom --test matrix -- netem_matrix --nocapture   # aplica cada perfil solo
set -euo pipefail

dev="${NETEM_DEV:-lo}"
loss="${NETEM_LOSS:-2%}"
delay="${NETEM_DELAY:-10ms}"
jitter="${NETEM_JITTER:-5ms}"

if ! command -v tc >/dev/null 2>&1; then
    echo "error: falta \`tc\` (paquete iproute2)" >&2
    exit 1
fi

# Aplica un perfil por su nombre.
profile() {
    case "$1" in
        lan)
            tc qdisc replace dev "$dev" root netem delay 1ms
            ;;
        buena)
            tc qdisc replace dev "$dev" root netem loss 0.5% delay 15ms 3ms
            ;;
        media)
            tc qdisc replace dev "$dev" root netem loss 2% delay 30ms 10ms
            ;;
        mala)
            tc qdisc replace dev "$dev" root netem loss 5% delay 60ms 20ms reorder 1%
            ;;
        cuello)
            # netem para el retardo y, debajo, tbf para la capacidad (8 Mbit/s, 200 ms de cola).
            tc qdisc replace dev "$dev" root handle 1: netem delay 10ms
            tc qdisc replace dev "$dev" parent 1:1 handle 10: tbf rate 8mbit burst 32kbit latency 200ms
            ;;
        *)
            echo "error: perfil desconocido «$1» (lan, buena, media, mala, cuello)" >&2
            exit 2
            ;;
    esac
    echo "netem con el perfil $1 en $dev"
}

case "${1:-}" in
    apply)
        tc qdisc replace dev "$dev" root netem loss "$loss" delay "$delay" "$jitter"
        echo "netem activo en $dev: pérdida $loss, retardo $delay ± $jitter"
        ;;
    profile)
        profile "${2:-}"
        ;;
    profiles)
        echo "lan buena media mala cuello"
        ;;
    clear)
        if tc qdisc show dev "$dev" | grep -q -E 'netem|tbf'; then
            tc qdisc del dev "$dev" root
            echo "netem retirado de $dev"
        else
            echo "no había netem en $dev"
        fi
        ;;
    status)
        tc qdisc show dev "$dev"
        ;;
    *)
        echo "uso: $0 apply|profile PERFIL|profiles|clear|status" >&2
        exit 2
        ;;
esac
