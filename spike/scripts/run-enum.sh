#!/usr/bin/env bash
# Serie completa del experimento 2 sobre C:\ (SOLO LECTURA). Ejecutar desde spike/ tras `cargo build --release -p enum-bench`.
# Intercala las APIs para que ninguna se beneficie siempre de la caché que dejó la anterior.
set -u
cd "$(dirname "$0")/.."
E=target/release/enum-bench.exe
mkdir -p out
$E --probe --json out/probe.json > /dev/null
for round in 1 2 3; do
  for api in extd find find-alloc; do
    $E --api $api --root 'C:\' --runs 1 --threads 32 --json out/enum_${api}_r${round}.json 2> out/enum_${api}_r${round}.log
    tail -1 out/enum_${api}_r${round}.log
  done
done
# Escalado de hilos (extd, 1 ejecución por valor; ya con caché caliente)
for t in 1 2 4 8 16 32 64 128; do
  $E --api extd --root 'C:\' --runs 1 --threads $t --json out/enum_extd_t${t}.json 2> out/enum_extd_t${t}.log
  echo "threads=$t: $(tail -1 out/enum_extd_t${t}.log)"
done
# 1 hilo, extd y find INTERCALADOS (2 rondas) y escalado de hilos seguido sobre el mismo árbol (series limpias)
mkdir -p out/t1
for i in 1 2; do for a in extd find; do
  $E --api $a --root 'C:\' --runs 1 --threads 1 --json out/t1/${a}_t1_r$i.json 2> /dev/null
done; done
for t in 2 4 8 16 32; do $E --api extd --root 'C:\' --runs 1 --threads $t --json out/t1/extd_t${t}_r2.json 2> /dev/null; done
# Validación de recuentos y sumas en un árbol pequeño y estable (las 4 APIs deben coincidir exactamente)
mkdir -p out/val
for a in std extd find find-alloc; do
  $E --api $a --root 'C:\Windows\System32\DriverStore' --runs 1 --threads 16 --json out/val/driverstore_$a.json
done
