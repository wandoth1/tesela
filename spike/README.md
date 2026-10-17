# Tesela phase 0 spike

**Throwaway measurement** code that turns the hypotheses of `docs/PLAN.md` §6 into data. It is not the product: do not
reuse anything from here without re-reading it. The findings are in `docs/phase-0-findings.md`. The raw data of
each measurement is **not versioned** (it describes the machine where it is taken): it stays local, in `spike/results/` (in `.gitignore`).

It is a **separate** Cargo workspace (`spike/Cargo.toml`), independent of the product workspace. Requirements:
Windows 10/11 x64, stable Rust (2024 edition) and the `x86_64-pc-windows-msvc` toolchain.
All dependency versions were verified with `cargo search` on 2026-10-15 and match PLAN.md §3.10.

```text
spike/
├─ common/        utilities: GetProcessMemoryInfo, percentiles, free space of the volume
├─ enum-bench/    experiment 2: parallel enumeration of C:\ (read-only) + probe of $MFT / FSCTL
├─ trash-bench/   experiment 3: Recycle Bin with the `trash` crate
├─ delete-bench/  experiment 4: POSIX deletion with FileDispositionInfoEx
├─ ui-treemap/    experiment 1: synthetic treemap of ~30k rectangles in egui (wgpu or glow)
├─ fonts-mica/    experiment 5: SegUIVar.ttf with variable axes and Mica on eframe
├─ scripts/       run-enum.sh, run-ui.sh, run-trash.sh: full batteries; collect-results.sh + redact.py: copy and (optional local denylist in spike/.denylist, not versioned)
│                 redact the data into results/; summarize.py: Markdown tables from the JSON files
└─ results/       (local, not versioned) JSON/CSV/PNG of your measurements
```

## Local denylist (`spike/.denylist`)

`scripts/collect-results.sh` copies the results from `out/` to `results/`, redacting personal data with
`scripts/redact.py`. The **main** protection is the **allowlist** of `redact.py` (only what it recognizes gets through). As
an additional check, the script searches what has already been redacted for a **local denylist**: `spike/.denylist`.

- **It is a per-machine file and is NOT versioned** (it is in `.gitignore`): it contains exactly the private names that must
  not be published, so uploading it to the repository would leak them.
- If it does not exist, the script warns (`AVISO: no existe spike/.denylist`, that is, "WARNING: spike/.denylist does not exist") and only checks the Windows user name and the
  SIDs.
- **How to recreate it** on another machine or after cloning: create `spike/.denylist` with **one regular expression (`grep -E`) per
  line**, no comments (each non-empty line is a pattern; empty ones are ignored). Put names of personal folders,
  of installed programs you do not want to reveal, the machine name, etc. Shape of the file, with generic values:

  ```text
  name-of-a-private-folder
  name-of-an-installed-program
  name-of-the-machine
  ```

- Before publishing `results/`, run `collect-results.sh` with the denylist created: it aborts if any match remains
  and does not modify `results/`.

## Build

From `spike/`:

```powershell
# File system experiments
cargo build --release -p enum-bench -p trash-bench -p delete-bench

# UI: the backend is chosen with a feature and they CANNOT be mixed in the same binary.
cargo build --release -p ui-treemap --no-default-features --features wgpu-all   # wgpu with all backends
cargo build --release -p fonts-mica --no-default-features --features wgpu
$env:CARGO_TARGET_DIR = "target-dx12"      # wgpu with ONLY the DX12 backend (as PLAN.md §1)
cargo build --release -p ui-treemap --no-default-features --features wgpu-dx12
$env:CARGO_TARGET_DIR = "target-glow"
cargo build --release -p ui-treemap -p fonts-mica --no-default-features --features glow
Remove-Item Env:CARGO_TARGET_DIR
```

The binaries end up in `target\release\` (wgpu, all backends), `target-dx12\release\` (wgpu DX12 only) and
`target-glow\release\` (glow). Those directories and `out\` are in `.gitignore`. Release profile: `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"`, as in the plan.

## Run each experiment

All of them write their results as JSON in `out/` (local). **Do not upload those results or screenshots to the repository**:
they contain data from your machine. The repository only publishes findings (`docs/phase-0-findings.md`).

### 1. `ui-treemap` (UI: RSS, exe, ms/frame)

```powershell
# Interactive (real hover, FPS on screen):
target\release\ui-treemap.exe
# Measurement without interaction: it closes by itself and writes the JSON (+ CSV of frames):
target\release\ui-treemap.exe --bench --label prueba --out out\ui\prueba.json --frames-csv out\ui\prueba.csv
# Options: --cells 30000  --phase-secs 4  --idle-secs 4  --vsync on|off  --mesh (a pre-tessellated Mesh)  --topmost
#           --backend dx12|vulkan|gl  --adapter "Microsoft Basic"   (wgpu only; "Microsoft Basic" = WARP; forcing it on a machine with a GPU does not replace the RDP/Hyper-V test: see the Pending section of docs/phase-0-findings.md)
# Full battery:
bash scripts/run-ui.sh
```

`--bench` phases: warm-up (1 s, discarded) → continuous (repaints everything every frame) → hover (a synthetic pointer that
sweeps the map: hit-test + highlight) → idle (no repainting; this is where the "idle" RSS is measured). **Do not touch the mouse nor
cover the window** during the measurement (a covered window stops painting).

### 2. `enum-bench` (enumeration of C:\\, read-only)

```powershell
target\release\enum-bench.exe --api extd|find|find-alloc|std --root 'C:\' --threads 32 --runs 3 --json out\x.json
target\release\enum-bench.exe --probe --json out\probe.json   # $MFT and FSCTL_GET_NTFS_VOLUME_DATA
bash scripts/run-enum.sh                                       # full series (3 interleaved rounds + thread scaling)
```

`--api std` is a single-threaded reference with `std::fs` to validate counts on small trees
(e.g. `--root 'C:\Windows\System32\DriverStore'`). The enumeration never writes anything.

### 3. `trash-bench` (Recycle Bin)

```powershell
target\release\trash-bench.exe run --n 100000 --loose-mode one --json out\trash.json
target\release\trash-bench.exe count      # number of Recycle Bin items and time of list()
target\release\trash-bench.exe cleanup        # only LISTS what it would do (recovery after a dead run)
target\release\trash-bench.exe cleanup --yes  # really purges and deletes; only with no other run alive
```

> **WARNING: `trash-bench run` puts up to ~100,000 transient items into the user's REAL Recycle Bin for a long
> time** (tens of minutes with `--n 100000`; with `--n 1000` it is fast). All of them are purged at the end. Do not run it if you do not want
> that, and do not interrupt the run with a `kill`: the internal guard only covers panics and errors (for the rest there is
> `cleanup`).

It works in `%TEMP%\tesela-spike-<uuid>`. It only restores or purges items whose origin is **under `%TEMP%` and whose first
component after `%TEMP%` starts with the full `tesela-spike-<uuid>`** (in `cleanup`: with `tesela-spike-`), and at the end it
checks that the set of Recycle Bin ids is identical to the one before. It validates all arguments before creating
anything. **It never empties the Recycle Bin.**
`--loose-mode` accepts `one` (a single `delete_all` call with everything), `batch=1000` (batches) and `par=8` (threads in parallel).

### 4. `delete-bench` (POSIX deletion)

```powershell
target\release\delete-bench.exe functional   --json out\a.json   # flags, read-only
target\release\delete-bench.exe share-delete --json out\b.json   # §2.6: file open by another handle (writes 8x512 MiB in %TEMP%)
target\release\delete-bench.exe reparse      --json out\r.json   # delete a directory junction/symlink: is the target respected?
target\release\delete-bench.exe speed --files 50000 --runs 3 --threads 16 --dirs 100 --json out\c.json
```

### 5. `fonts-mica` (variable fonts and Mica)

```powershell
target\release\fonts-mica.exe --mica on --shot-egui out\f-egui.png --shot-desktop out\f-desktop.png --out out\fm.json
# --mica off (control), --presentation visual|hwnd (DX12), SPIKE_BACKEND=dx12|vulkan|gl (wgpu only)
```

The window is transparent, always on top and placed at (120,120): **do not cover it** while the desktop screenshot is taken
(GDI, `BitBlt` of the composed desktop, the only way to see the backdrop that DWM paints).

## What can NOT be measured here (pending for the user)

RDP and Hyper-V without a GPU, and Windows 10 22H2. The instructions are in `docs/phase-0-findings.md` (Pending section).
