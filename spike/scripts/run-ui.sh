#!/usr/bin/env bash
# GPU_ADAPTER: subcadena del nombre de tu GPU dedicada (p. ej. export GPU_ADAPTER="..."); no se versiona.
: "${GPU_ADAPTER:?define GPU_ADAPTER con parte del nombre de tu GPU}"
# Matriz del experimento 1. Ejecutar desde spike/ tras compilar ambos backends:
#   cargo build --release -p ui-treemap --no-default-features --features wgpu-all
#   CARGO_TARGET_DIR=target-glow cargo build --release -p ui-treemap --no-default-features --features glow
#   CARGO_TARGET_DIR=target-dx12 cargo build --release -p ui-treemap --no-default-features --features wgpu-dx12
# Cada ejecución abre una ventana de 1280x800 unos 15 s y escribe un JSON en out/ui/. No tocar el ratón ni tapar la ventana.
set -u
cd "$(dirname "$0")/.."
mkdir -p out/ui
W=target/release/ui-treemap.exe
G=target-glow/release/ui-treemap.exe
D=target-dx12/release/ui-treemap.exe   # wgpu compilado SOLO con el backend DX12 (PLAN.md §1)
run() { # etiqueta exe args...
  local label=$1 exe=$2; shift 2
  echo "== $label"
  # La salida va a un archivo (no a una tubería): con glow, una tubería provocó cuelgues intermitentes en las pruebas.
  # `-s KILL` porque TERM no mata de forma fiable un proceso gráfico nativo desde Git Bash.
  timeout -s KILL 150 $exe --bench --label "$label" --out "out/ui/$label.json" --frames-csv "out/ui/$label.csv" --topmost "$@" > "out/ui/$label.log" 2>&1
  echo "   exit=$? $(grep -E '^resultados' out/ui/$label.log || echo SIN-RESULTADO)"
}
for r in 1 2 3; do
  run "wgpu-vulkan-vsync-r$r" $W
  run "glow-vsync-r$r" $G
done
for r in 1 2; do
  run "wgpu-vulkan-novsync-r$r" $W --vsync off
  run "glow-novsync-r$r" $G --vsync off
done
for r in 1 2; do
  run "wgpu-dx12-gpu-vsync-r$r" $W --backend dx12 --adapter "$GPU_ADAPTER"
  run "wgpu-dx12-warp-vsync-r$r" $W --backend dx12 --adapter "Microsoft Basic"
done
run "wgpu-dx12-gpu-novsync-r1" $W --backend dx12 --adapter "$GPU_ADAPTER" --vsync off
run "wgpu-dx12-warp-novsync-r1" $W --backend dx12 --adapter "Microsoft Basic" --vsync off
run "wgpu-gl-vsync-r1" $W --backend gl
# Alternativa de dibujo: UN mesh pretesselado reutilizado (en vez de 30k RectShape por frame), y el tope del plan (15k)
for r in 1 2 3; do
  run "wgpu-vulkan-mesh-vsync-r$r" $W --mesh
  run "glow-mesh-vsync-r$r" $G --mesh
done
run "wgpu-vulkan-mesh-novsync-r1" $W --mesh --vsync off
run "glow-mesh-novsync-r1" $G --mesh --vsync off
run "wgpu-vulkan-15k-vsync-r1" $W --cells 15000
run "glow-15k-vsync-r1" $G --cells 15000
run "wgpu-dx12-warp-mesh-vsync-r1" $W --backend dx12 --adapter "Microsoft Basic" --mesh
# wgpu solo-DX12 (exe más pequeño): vsync, mesh y línea base; sobre la GPU dedicada (GPU_ADAPTER) y sobre WARP
for r in 1 2 3; do
  run "wgpu-dx12only-vsync-r$r" $D
  run "wgpu-dx12only-mesh-vsync-r$r" $D --mesh
done
run "wgpu-dx12only-novsync-r1" $D --vsync off
run "wgpu-dx12only-warp-vsync-r1" $D --adapter "Microsoft Basic"
for r in 1 2; do
  run "base100-wgpu-dx12only-r$r" $D --cells 100
done
# Líneas base con casi nada que dibujar: aíslan el coste fijo (driver, ventana, egui) del coste del treemap
for r in 1 2; do
  run "base100-wgpu-vulkan-r$r" $W --cells 100
  run "base100-glow-r$r" $G --cells 100
done
run "base100-wgpu-dx12-warp-r1" $W --backend dx12 --adapter "Microsoft Basic" --cells 100
run "base100-wgpu-dx12-gpu-r1" $W --backend dx12 --adapter "$GPU_ADAPTER" --cells 100
