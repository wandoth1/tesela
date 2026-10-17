# Phase 0: findings of the measurement spike

Phase 0 used test binaries (`spike/`) to measure the technical hypotheses of the [plan](PLAN.md) before writing the product. The decisions are already incorporated in the plan (round 4 of §9). This document records **what was checked and what was concluded**.

> **Privacy.** The raw measurement data (JSON, CSV and screenshots) is **not published**, because it describes the machine where it was taken. Here there are only application-level results (executable size, process memory, cost per frame) and relative comparisons. To reproduce the measurements on your machine, see [spike/README.md](../spike/README.md); the results stay in `spike/results/`, which is in `.gitignore`.

All measurements were made on Windows, without administrator rights, on a single test machine. They are indicative, not a benchmark.

## Summary

| Topic | Result | Decision |
|---|---|---|
| UI backend | Exe: **glow 6.3 MB**, **wgpu DX12 only 10.5 MB**, wgpu with all backends 12.3 MB. Process memory (private working set) with a 30k-cell map: glow ≈ 60–65 MiB, **wgpu DX12 ≈ 70 MiB**, wgpu Vulkan ≈ 110 MiB | **wgpu DX12 only** (WARP as fallback for machines without a GPU). Vulkan excluded. glow as plan B |
| Drawing the treemap | One `RectShape` per cell with 30k cells: ~16 ms per frame, no headroom for 60 fps. **A `Mesh` tessellated once and reused: ~1–2 ms per frame** (≈ 10× less) | The treemap is drawn as a cached `Mesh`. Target: p99 CPU per frame < 8 ms |
| Squarified layout | 30k cells in ~0.5 ms; building the `Mesh`, ~0.4–0.8 ms | Enough for interactive relayouts |
| Mica (translucent background) | Works with glow; **not with wgpu** (DX12-HWND does not support transparency; with DX12-Visual and Vulkan the window comes out white) | Solid background |
| Fonts | `SegUIVar.ttf` is loaded from the system and egui applies the `wght` axis (300–700); `opsz` is not applied on its own | Load the system fonts at run time |
| Icons | Fluent UI System Icons: **MIT** license | They can be embedded |
| API to read folders | `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` is **as fast or slightly faster** than `FindFirstFileExW` and also gives `AllocationSize` and `FileId`. `FindFirstFileExW` + opening each file to ask for its size on disk is **~9× slower** and fails on system files | `FileIdExtdDirectoryInfo` as the main one; `FindFirstFileExW` as fallback, with estimated size on disk |
| Parallelism | Speed rises with the first threads and then barely improves (several times faster than with 1 thread) | Threads tunable by measuring |
| Size on disk vs logical size | Negligible global difference; estimating by rounding to the cluster deviates negligibly | Size on disk is shown |
| Hard links | Without deduplicating it overcounts: a small percentage of the volume, but a considerable fraction inside `C:\Windows`, where links abound. The `FindFirstFileExW` fallback path gives no `FileId` and **does not deduplicate hard links**. Sorting the FileIds is negligible compared with the scan | Always deduplicate by FileId |
| Unexplained space | What is enumerated without administrator stays below `total − free`, and **deduplicating hard links INCREASES the unexplained gap** | "Windows and other system data" block |
| NTFS metadata | `$MFT` and friends **do not appear** when enumerating. `FSCTL_GET_NTFS_VOLUME_DATA` works without administrator with a handle to the root directory | The size of the MFT can be shown without elevating |
| Recycle Bin, 100k files | Sending a folder of 100k files takes **~2× the plan's 10 s threshold**, that is, 10 s is roughly enough for half of the 100k files (a single measurement). File by file is not viable: tens of times slower than sending the whole folder | Development folders: destination **deferred to v0.2** (see below) |
| Restoring from the Recycle Bin | `trash::os_limited::restore_all` restores only what is indicated, correctly. `list()` is linear in the size of the **WHOLE Recycle Bin** (tens of seconds with 100k items). `delete_all` can return an error having moved almost everything | Undo verified file by file; only for small batches or whole folders |
| POSIX deletion (`FileDispositionInfoEx`) | It works, but is **not faster** than `std::fs::remove_file`, which already falls back to POSIX + `IGNORE_READONLY` with read-only files | Discarded: `remove_file` / `remove_dir` are used |
| File open with `FILE_SHARE_DELETE` | With **any** deletion method, the name disappears but **the space is not freed** until the other process closes the file | Detect the open browser; count only what was really freed |
| Deleting a junction | `remove_dir`, `remove_dir_all` and `FileDispositionInfoEx` with `FILE_FLAG_OPEN_REPARSE_POINT` delete **the link, not the target** (target intact 4/4); `remove_file` and `DeleteFileW` fail | A link is never entered |

## Development folders (`node_modules`, `target`, `.venv`…)

The plan's rule said: if sending 100k files to the Recycle Bin takes more than 10 s, they go to permanent deletion. The measurement exceeded it, but it was taken **only once and with system interference**. Moreover, permanent deletion "succeeds" even if there are open files (`FILE_SHARE_DELETE`), and that is exactly what happens in a project open in an editor.

Decision: **deferred to v0.2**, which is when these folders enter the product. There we will:
- repeat the measurement ≥ 3 times with a realistic tree;
- try **"rename first"**: renaming the folder to a sibling tombstone name fails if any file is open (it fails closed), and then the renamed folder is deleted. In a separate, unversioned check, renaming failed with open files with or without `FILE_SHARE_DELETE`.

## Pending

Tests that could not be done on the measurement machine:
- **RDP and Hyper-V without a GPU.** Check that the wgpu DX12 build starts and draws (and glow for comparison). Forcing WARP on a machine with a GPU is not a substitute.
  1. Build the two builds (see `spike/README.md`). Copy `ui-treemap.exe` from `target-dx12` and from `target-glow` to the remote machine and **rename them** to `ui-treemap-wgpu.exe` and `ui-treemap-glow.exe`, because both have the same name.
  2. Run in each environment: an RDP session without a GPU, and the console of a Hyper-V VM without a GPU, both in **basic session** and in **enhanced session**:
     ```powershell
     .\ui-treemap-wgpu.exe --bench --topmost --label rdp-wgpu --out rdp-wgpu.json
     .\ui-treemap-wgpu.exe --bench --topmost --mesh --label rdp-wgpu-mesh --out rdp-wgpu-mesh.json
     .\ui-treemap-glow.exe --bench --topmost --label rdp-glow --out rdp-glow.json
     .\ui-treemap-glow.exe --bench --topmost --mesh --label rdp-glow-mesh --out rdp-glow-mesh.json
     ```
  3. Note, for each run: whether it starts and draws or, if it fails, the error message; the adapter and backend that appear in the JSON; p50 and p99 CPU per frame; fps; and the idle memory (private working set).
  4. Criterion: if wgpu starts and draws in all environments, wgpu stays. If it fails and glow works, switch to glow. If **both fail**, it is a bigger problem and is decided separately.
  5. Those JSON files contain machine data (adapter, driver, build…): **they are not uploaded to the repository**; only the conclusions are noted.
- **Windows 10 22H2:** `FileIdExtdDirectoryInfo` and `SegUIVar.ttf`.
- **Directory symlinks** when deleting. Creating them needs Developer Mode or administrator rights.
- `FOF_WANTNUKEWARNING` together with `FOF_NO_UI`, and the real Recycle Bin limit applied by Windows.
