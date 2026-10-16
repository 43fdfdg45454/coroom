#!/usr/bin/env bash
# Convierte el informe de avisos de tiempo de las pruebas (`COROOM_TIMING_REPORT`, una línea
# `fichero:línea<TAB>prueba<TAB>mensaje` por umbral superado; ver crates/testkit) en anotaciones de
# GitHub Actions y en una tabla del resumen del job. Nunca falla: los umbrales de tiempo informan,
# no bloquean.
#
# Uso: tools/timing-report.sh INFORME
set -uo pipefail

report="${1:?uso: tools/timing-report.sh INFORME}"
summary="${GITHUB_STEP_SUMMARY:-/dev/stdout}"

if [ ! -s "$report" ]; then
  echo "Ninguna prueba de tiempo se salió de su umbral."
  echo "### Pruebas de tiempo: todas dentro de su umbral" >> "$summary"
  exit 0
fi

count=$(wc -l < "$report" | tr -d ' ')
echo "$count avisos de tiempo"
{
  echo "### Pruebas de tiempo: $count fuera de umbral"
  echo
  echo "No hacen fallar la CI: miden la máquina además del código. Si se repiten, hay que mirarlas."
  echo
  echo "| Prueba | Dónde | Medida |"
  echo "|---|---|---|"
} >> "$summary"

while IFS=$'\t' read -r place test message; do
  file="${place%:*}"
  line="${place##*:}"
  # Las rutas de Windows llegan con barras invertidas; las anotaciones quieren las del repositorio.
  file="${file//\\//}"
  # Comandos de flujo de trabajo: `%`, salto de línea y, en las propiedades, `:` y `,` se escapan
  # (los nombres de las pruebas unitarias llevan `::`).
  data="${message//%/%25}"
  title="Umbral de tiempo ($test)"
  title="${title//%/%25}"; title="${title//:/%3A}"; title="${title//,/%2C}"
  prop_file="${file//%/%25}"; prop_file="${prop_file//:/%3A}"; prop_file="${prop_file//,/%2C}"
  echo "::warning file=$prop_file,line=$line,title=$title::$data"
  echo "| \`$test\` | \`$file:$line\` | ${message//|/\\|} |" >> "$summary"
done < "$report"
exit 0
