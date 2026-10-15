#!/usr/bin/env bash
# Uso LOCAL. Copia a spike/results/ los JSON/CSV de spike/out/ y los redacta con scripts/redact.py.
# Los resultados NUNCA se publican: contienen datos del equipo (adaptador, driver, build de Windows, tamaños de
# volumen...). La redacción es solo defensiva; results/ está en .gitignore y no debe subirse al repositorio.
# Los PNG de capturas del escritorio con Mica DESACTIVADO se excluyen a propósito: al ser la ventana transparente,
# muestran lo que hubiera detrás.
# Se niega a borrar results/ si out/ no existe o está vacío, y aborta si tras redactar queda alguna palabra de la
# lista negra (nombres de carpetas/programas del usuario, usuario de Windows, ruta del repositorio local).
set -eu
cd "$(dirname "$0")/.."
if [ ! -d out ] || [ -z "$(ls -A out 2>/dev/null)" ]; then
  echo "ERROR: spike/out/ no existe o está vacío; no se toca spike/results/" >&2
  exit 1
fi
R=results
TMP=results.tmp
rm -rf "$TMP"; mkdir -p "$TMP/enum" "$TMP/ui" "$TMP/trash" "$TMP/delete" "$TMP/fonts-mica"
redact() { python scripts/redact.py "$1" "$2"; }
for f in out/probe.json out/c_extd_warmup.json out/enum_*.json out/val/*.json out/t1/*.json; do [ -f "$f" ] && redact "$f" "$TMP/enum/$(basename "$f")"; done
for f in out/ui/*.json out/ui/*.csv; do [ -f "$f" ] && redact "$f" "$TMP/ui/$(basename "$f")"; done
for f in out/trash_n*.json out/trash_batch*.json out/trash_par*.json; do [ -f "$f" ] && redact "$f" "$TMP/trash/$(basename "$f")"; done
for f in out/delete_*.json; do [ -f "$f" ] && redact "$f" "$TMP/delete/$(basename "$f")"; done
for f in out/fm/*.json out/fm/log-*.log; do [ -f "$f" ] && redact "$f" "$TMP/fonts-mica/$(basename "$f")"; done
for f in glow-mica-on-desk glow-mica-on-egui wgpu-vk-mica-on-desk wgpu-vk-mica-on-egui wgpu-dx12-visual-mica-on-desk; do cp "out/fm/$f.png" "$TMP/fonts-mica/" 2>/dev/null || true; done
[ -f out/xcheck.json ] && redact out/xcheck.json "$TMP/ui/xcheck-getprocess.json"
[ -f out/xcheck-getprocess.txt ] && redact out/xcheck-getprocess.txt "$TMP/ui/xcheck-getprocess.txt"
[ -f out/crate-versions.txt ] && cp out/crate-versions.txt "$TMP/crate-versions.txt"

# Comprobación negativa: ninguna palabra de la lista negra en el contenido redactado. La lista negra son los nombres
# privados del usuario (carpetas, programas...) y vive en spike/.denylist (UNA expresión por línea, NO versionado,
# para no publicar justo lo que se quiere ocultar). Además se busca siempre el nombre de usuario de Windows y los SID.
DENY="S-1-5-21-[0-9]"
[ -n "${USERNAME:-}" ] && DENY="$DENY|$USERNAME"
if [ -f .denylist ]; then DENY="$DENY|$(grep -v '^$' .denylist | paste -sd'|')"; else echo "AVISO: no existe spike/.denylist (lista negra local); solo se comprueban usuario y SID" >&2; fi
if grep -rIiE "$DENY" "$TMP" | head -5 | grep .; then
  echo "ERROR: quedan datos personales tras redactar (ver arriba); results/ NO se ha modificado" >&2
  rm -rf "$TMP"; exit 1
fi
rm -rf "$R"; mv "$TMP" "$R"
echo "results/: $(find $R -type f | wc -l) archivos, $(du -sk $R | cut -f1) KiB"
