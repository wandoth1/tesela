#!/usr/bin/env bash
# Experimento 3. Puede tardar mucho (decenas de minutos con 100k archivos sueltos). Desde spike/.
# Todo ocurre en %TEMP%\tesela-spike-<uuid>; la Papelera real del usuario no se toca (se verifica al final de cada ejecución).
set -u
cd "$(dirname "$0")/.."
T=target/release/trash-bench.exe
mkdir -p out
$T count
# Escalado (para extrapolar y detectar no linealidades)
for n in 1000 5000 10000; do
  $T run --n $n --restore-sample 200 --json out/trash_n$n.json > /dev/null 2> out/trash_n$n.log
done
# Variantes de la llamada con archivos sueltos (solo sueltos, 10k)
$T run --n 10000 --skip folder --loose-mode batch=1000 --restore-sample 200 --json out/trash_batch1000_n10000.json > /dev/null 2> out/trash_batch1000_n10000.log
$T run --n 10000 --skip folder --loose-mode par=8 --restore-sample 200 --json out/trash_par8_n10000.json > /dev/null 2> out/trash_par8_n10000.log
# El dato que pide el plan: 100k archivos pequeños, carpeta entera y sueltos (una sola llamada)
$T run --n 100000 --restore-sample 1000 --json out/trash_n100000.json > /dev/null 2> out/trash_n100000.log
$T count
echo TRASH-DONE
