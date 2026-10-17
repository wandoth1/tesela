# Plan: visual disk space analyzer for Windows (Rust)

> **Status:** design **closed and validated** (2 rounds between UX design and architecture + validation by the advisor; see §9). **Phase 0 completed** ([findings](phase-0-findings.md)); its numbers are already incorporated here (round 4 in §9). The only open items are the user's pending checks in §6. Next: MVP v0.1.
> **Working name:** **Tesela**, the single piece of a mosaic, like each block of the treemap. Checked on 2026-10-15: there is no `tesela` crate on crates.io, and on GitHub the only match is `raycoarana/tesela`, an unrelated Android library from 2015. The name is free on crates.io and winget (checked on 2026-10-15); registered trademarks not reviewed.

---

## 1. Key decisions

| Topic | Decision | Why |
|---|---|---|
| Language | Stable Rust; the MSRV is set by egui (today 1.95), with `rust-toolchain.toml` | Fast, low RAM and a single binary |
| UI | **egui/eframe 0.36** with a custom theme. **`wgpu` backend compiled with DX12 only** (WARP as the fallback for RDP and virtual machines without a GPU). Vulkan is excluded. The treemap is drawn as a **`Mesh` tessellated once and cached**. `glow` is plan B only if the RDP/Hyper-V test (§6) fails with wgpu and works with glow | Measured in phase 0: exe ≈ 10.5 MB; ≈ 70 MiB private working set with the map; with `Mesh`, ~1–2 ms per frame. MIT/Apache, compatible with SignPath; AccessKit. Slint is ruled out because of its license (GPL or non-OSI). WinUI via `windows-reactor` needs the Windows App Runtime and is still in preview |
| UI plan B | Win32 + Direct2D | Smaller, but it means months of widgets, keyboard and UIA work |
| Visualization | **Squarified treemap + list with bars**, kept in sync. No sunburst | The treemap shows the shape of the disk; the list gives exact figures and is the accessible view |
| Size shown | **On disk** (`AllocationSize`) as the main value; the logical size also appears in the details panel | It is what is actually freed. Explorer shows the logical size, so both are shown |
| Scan in the MVP | Own parallel walker with `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` and `FindFirstFileExW` as the fallback | Gets `AllocationSize` and `FileId` in batches. `walkdir` is single-threaded and `jwalk` is deprecated |
| Fast scan (v0.3) | `$MFT` read with `ntfs-reader` (streaming `MftScan`) behind a `trait MftSource` | WizTree style, but it requires administrator rights |
| Elevation | `asInvoker` manifest: it **never** asks for UAC at startup. Optional "Fast mode (administrator)" button | No friction. In v1.0, an elevated helper that only scans |
| Deletion | **Destination set by each rule, independent of risk** (see §2.5). Everything goes through the basket | Sending temporary files to the Recycle Bin frees nothing |
| OS | Official support for Windows 10 22H2 and Windows 11; technical floor 1809. x64 from the MVP; ARM64 is built in CI from the MVP and published in v1.0 | Rust requires Win10+. `aarch64-pc-windows-msvc` is Tier 1 |
| Deliverable | **Per-user MSI installer from v0.1** (WiX): no administrator, installs to `%LOCALAPPDATA%\Programs\Tesela`, Start menu shortcut, shows up in Installed apps, updates in place and is signed with SignPath. Portable zip as a secondary option. MSIX ruled out | User decision (2026-10-15). Consistent with "never UAC". MSIX isolates the file system, which clashes with reading the whole disk and the MFT |
| License | MIT / Apache-2.0 dual | Rust ecosystem and a SignPath requirement |
| Out of scope | Telemetry, accounts, "registry optimizer", uninstaller, sunburst, shredding, rules that delete without intervention | Noise with no value |

---

## 2. Product and UX

### 2.1 Principles
1. **Three clicks to free space:** Analyze → "Clean the safe stuff" → Confirm.
2. **The answer first, then the detail.** Within seconds "You can free at least X GB" appears, and then the map.
3. **Nothing is deleted without seeing the total and the destination** (permanent or Recycle Bin).
4. **Risk is seen before it is read:** color with an icon and a shape, never color alone.
5. **Everything is explained in one sentence:** every suggestion carries its "why".
6. **Light and honest:** if something is not clearly safe, it is **not preselected**. Only what has really been freed is counted as freed.

Terminology: the UI always says "files" and "Recycle Bin", as Windows does.

### 2.2 Flow
1. **Home:** one card per drive with a usage ring and the line "Last cleanup: 3 days ago · measured change in free space: 8 GB", which only appears if there is history.
2. **Scan:** two zones (§2.4).
3. **Exploration:** Map, List and Recommendations.
4. **Basket** grouped by destination.
5. **Confirmation:** "Free up to 11.0 GB and move 3.2 GB to the Recycle Bin".
6. **Summary:** "Freed now: 11.0 GB", followed by "3.2 GB is in the Recycle Bin · [Empty now] [Open the Recycle Bin]". "Empty now" becomes the primary button if what is in the Recycle Bin exceeds what has already been freed.

When cleaning finishes **there is no rescan**: the blocks shrink with an animation and the totals are recalculated (§3.6).

### 2.3 Main window
```
┌──────────────────────────────────────────────────────────────────────────────┐
│ [≡] Tesela   C: › Users › ana › AppData     [Search… Ctrl+F] [Color by: Type ▾] [⚙] │
├────────────┬─────────────────────────────────────────────┬───────────────────┤
│ DRIVES     │   ┌─────────────┬──────────┬───────────┐   │ DETAILS           │
│ ▣ C: 400/500│   │ AppData     │ Videos   │ Downloads │   │ Videos            │
│ ▢ D:200/1000│   │  32 GB      │ 28 GB    │ 12 GB     │   │ On disk  28.3 GB  │
│ VIEWS      │   │             │          ├───────────┤   │ Size     28.1 GB  │
│ ◉ Map      │   │             │          │ Docs 8 GB │   │ 142 files         │
│ ○ List     │   ├─────────┬───┴──────────┤           │   │ Mod.: 92 % < 2022 │
│ ○ Recomm.  │   │ node_mod│ Windows and other data ⓘ │   │ Risk: Review      │
│   (14 GB)  │   └─────────┴──────────────────────────┘   │ [Explorer][Path]  │
├────────────┴─────────────────────────────────────────────┴───────────────────┤
│ BASKET: 23 items · 14.2 GB       [View] [Empty]          [Clean 14.2 GB ▸]   │
├──────────────────────────────────────────────────────────────────────────────┤
│ 400 GB used of 500 GB · 1.0 M files · scanned 2 min ago         [Rescan]     │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 2.4 Scan screen (early verdict)
```
┌ While we scan your disk… ────────────────────────────────────────────┐
│ You can free at least 9.0 GB             [ Clean the safe stuff ]    │
│ (updates as it goes)          ◌ Checking 2 of 5 places…              │
│  ✓ Windows temporary files ...... 2.0 GB                             │
│  ✓ Browser cache ................ 7.0 GB                             │
│  ✓ Recycle Bin .................. 0.5 GB   (Review)                  │
│  ◌ Project folders .............. searching…                         │
│ ──────────────────────────────────────────────────────────────────── │
│ Map: Users ████████ · Windows ██ · …               [Stop and view]   │
└──────────────────────────────────────────────────────────────────────┘
```
- Known paths are scanned first, through a priority queue (§3.3). Each row shows its final figure when it finishes and does not change again.
- "Clean the safe stuff" becomes active when the sweep of the known paths ends.
- The headline **adds up only the Safe rows**, which is exactly what "Clean the safe stuff" does. In the example the Recycle Bin (Review) is shown but does not count.
- While rows are still pending, the headline says "**at least**". When it finishes it changes to "You can free 14.2 GB" with a counter animation. If something turns up later, it is shown with a "+3.1 GB found" chip.
- **Stop and view:** the tree is built with what has been received up to that moment. Folders that were not walked in full show "≥ X GB" with faint diagonal stripes, the only pattern active by default. If one of them goes to the basket, the `cleaner` measures it again before confirming.

### 2.5 Risk and destination: two separate axes
**Risk** (Safe / Review / Info only):
- 🟢 Shield with a check: can be preselected and is included in "Clean the safe stuff".
- 🟠 Eye: never preselected.
- 🔺 "Info only" triangle: no checkbox.

**Destination** (its own icon, no risk colors):
- 🗑̸ **Permanent:** only regenerable items **classified by a rule**. Basket phrase: "Deleted, and it is recreated on its own".
- 🗑 **Recycle Bin:** everything else. Phrase: "Goes to the Recycle Bin · you can recover it".
- If something does not fit in the Recycle Bin (it exceeds the volume's real limit, or it is on a network or removable drive), a third amber group appears: "**Will be deleted permanently because it cannot be sent to the Recycle Bin**", with an explicit checkbox. This is recalculated **at execution time**, not when it is checked; if it changes, confirmation is requested again.
- Advanced setting "**Always send everything to the Recycle Bin**".
- **Execution order:** if "Empty the Recycle Bin" and items with a Recycle Bin destination are in the basket together, the Recycle Bin is emptied **first** and then the items are moved. That way what the user wants to keep recoverable is not deleted in the same step.
- What the user drags in by hand or is not classified ("Your file") has Review risk and a Recycle Bin destination, and is **never** deleted permanently except through the checkbox case above. The confirmation shows the 5 largest items and the phrase "These are your own files; they are not recreated".

### 2.6 Cleanup assistant
| Category | Risk | Destination | Version |
|---|---|---|---|
| Temporary files (`%TEMP%`, `Windows\Temp` if accessible), **only those not modified in the last 7 days** (`min_age_hours = 168`: the most recent of the creation, write and change dates; it is the `cleanmgr` criterion, "not modified in the last week"; "why": "Temporary files not modified in the last week; recent ones are left in case an installer is using them") | 🟢 | Permanent (files in use are skipped) | MVP |
| Browser cache (Chrome, Edge, Firefox): **only specific cache directories** (Chromium: `Cache`, `Code Cache`, `GPUCache`, `ShaderCache`/`GrShaderCache`; Firefox: `cache2` and `startupCache` under `%LOCALAPPDATA%`). **The browser profile is never cache**: history, bookmarks, cookies, credentials and persistent storage are never candidates | 🟢 | Permanent | MVP |
| Per-user Windows error reports (`ReportArchive` and `ReportQueue` under `%LOCALAPPDATA%\Microsoft\Windows\WER`), only children older than 7 days (`min_age_hours = 168`) | 🟠 Review, never preselected | Recycle Bin only; if an item cannot fit, leave it in place | v0.1.5 |
| Application crash dumps (`%LOCALAPPDATA%\CrashDumps`), only children older than 7 days (`min_age_hours = 168`) | 🟠 Review, never preselected | Recycle Bin only; if an item cannot fit, leave it in place | v0.1.5 |
| Recycle Bin ("Empty") | 🟠 | Permanent (it is the action itself). **Enumerated scope:** only the list of items captured at confirmation is purged; what arrives afterwards is not touched and is reported | MVP |
| `pagefile.sys`, `hiberfil.sys` (explained only: shrinking or disabling it requires a command prompt **as administrator**; `powercfg /h off` also removes hybrid sleep and fast startup; `powercfg /h /type reduced` shrinks it and keeps fast startup; Tesela runs nothing) | 🔺 | — | MVP |
| `node_modules`, `target`, `.venv`, `__pycache__`, `.gradle`, detected by a neighboring file (`package.json`, `Cargo.toml`, `pyvenv.cfg`…) | 🟢 | **Recycle Bin** by default ("It can be reinstalled with one command. It goes to the Recycle Bin in case you had local changes"). **Permanent destination DEFERRED to v0.2:** phase 0 measured ~2× the 10 s threshold for 100k files, but in a single measurement. In v0.2 it is repeated ≥ 3 times with a realistic tree, and **"rename first"** is tested: renaming the folder to a sibling tombstone name fails if there are open files (it stops safely) and then the renamed folder is deleted | v0.2 |
| Old downloads (> 6 months), old installers | 🟠 | Recycle Bin | v0.2 |
| Large files (> 500 MB) | 🟠 (only highlighted) | Recycle Bin | v0.2 |
| Duplicates (size → partial hash → full hash; one copy always remains) | 🟠 | Recycle Bin | v1.0 |
| WinSxS, restore points | 🔺 (DISM / settings advice; the app runs nothing) | — | v1.0 |

**Browser open:** `chrome.exe`, `msedge.exe` and `firefox.exe` are detected. If the browser is open, its row is **not preselected** and is not included in "Clean the safe stuff". The row says "**Close Chrome to free up to 1.9 GB**" and offers [Check again]. The reason is that Chromium opens its cache with `FILE_SHARE_DELETE`: with the browser open the deletion "works", but frees nothing until Chrome releases the files, and it also corrupts the browser's index. Phase 0 measured that this also happens with `std::fs::remove_file` and `DeleteFileW`, not just with POSIX: the name disappears and the space stays held. The Chrome case is an extrapolation from a synthetic handle. That is why a cache in use is not cleaned. The app never closes the browser. Before cleaning, figures are given as "about" or "up to".

**OneDrive:** files that exist only in the cloud cannot be selected to free space. If a synced file is deleted, the confirmation warns: "Synced with OneDrive: deleting it also removes it from the cloud".

### 2.7 Protection
- **Untouchable as a target** (their contents can be cleaned): the profile root, `AppData`, `AppData\Local`, `Roaming`, `LocalLow`, `C:\`, `C:\Users` and the ancestors of denied paths.
  - On the map they have a dotted border and a padlock.
  - In the menu, "Add to basket" appears disabled with a text explaining why; "View contents" takes its place.
  - If they are dragged to the basket, they bounce back.
- **Denied entirely:** `C:\Windows` (except the known temporary files), `Program Files`, `ProgramData` (except the known caches), `System Volume Information`, `$Recycle.Bin` (except the "Empty" action), other users' profiles, `C:\Users\Default`, `C:\Users\Public`, the `$` metadata files and the app's own folder.
- **Known-folder roots** (Documents, Desktop, Videos…): they are added as "**Contents of Videos**", without including the folder itself. Review risk, Recycle Bin destination.
- **Folders that contain something protected:** they are not accepted as a whole target (they stay "untouchable": padlock and bounce); their unprotected contents can be cleaned. This is safer than accepting them with a note, and it is what the audited code does.
- **Normalized basket:** if a node is added whose ancestor is already in the basket, it is not duplicated; if an ancestor is added, it absorbs its descendants.
- **The real defense is the `cleaner` crate, not the UI.** It computes the effective set and, right before deleting each item, validates the protected path again, checks that it is not a reparse point and that the size has not changed much (TOCTOU). This applies to both the Recycle Bin and permanent deletion.
- Files in use or without permission are skipped, counted and explained in the summary.

### 2.8 Unexplained space
- There is a synthetic gray block, sorted by size like one more child of the root: "**Windows and other system data · 40 GB**", with an ⓘ icon. It is computed as `total − free − scanned` using magnitudes **of the volume** (`NtQueryVolumeInformationFile(FileFsFullSizeInformation)`), measured when the scan finishes. The user's quota is shown separately ("Available to your account") and is never mixed into the remainder; without the query and with a quota, there is no block.
- It is clamped to ≥ 0 and hidden if it is under 1 %. It only appears in the scan of a whole volume and is marked "approximate".
- Tooltip: "Includes Windows files, hibernation, restore points and folders your user cannot see. This is normal and cannot be deleted from here". Link "See the detail (administrator mode)".
- If the gap exceeds 5 %, the status bar shows "The map explains 360 of 400 GB. What about the rest?". Separately, "N folders without permission" is shown.

### 2.9 Treemap and color
- It shows one level of children and a faint hint of the next, with 4 px corners and 2 px spacing. The label only appears if it fits.
- **Two distinct layers:**
  - Interactive grouping: blocks smaller than 24×24 px are merged into "Others (1,000 items, 400 MB)", which can be opened. It is the suffix of the sorted slice.
  - Drawing LOD: nothing is subdivided below about 3×3 px and there is a cap of ~15k rectangles per frame.
- Navigation: 180 ms animated zoom, breadcrumb, tooltip and context menu. In the MVP the menu has Open in Explorer, Copy path, Add to basket and Properties; "Exclude" arrives in v1.0.
- **Color by Type** (Okabe-Ito):
  - Video `#0072B2`, images `#E69F00`, audio `#CC79A7`, documents `#009E73`, archives `#D55E00`, programs `#56B4E9`, other `#8A8F98`.
  - A folder takes the color of its dominant type **only if it exceeds 50 % of the bytes**. Otherwise it is "**Mixed**", a lighter bluish gray, distinct from "Other".
  - A 4 px bottom strip shows the top 3 types when the block is ≥ 48 px. The tooltip breaks down the percentages.
- **Color by Age:** uses 5 bands by mtime (< 1 month, 6 months, 1 year, 3 years, older). A folder takes the band where 80 % of its bytes fall. Tooltip: "92 % of this has not been modified since 2022". It always says "modified", never "used".
- **Color by Risk:** only items that have a recommendation are colored.
- **Grays with meaning:**
  - "Info only": triangle, cannot be selected.
  - System block: ⓘ.
  - "Mixed": with its strip.
  - The system block and the "info only" items stay gray in all modes.
- After a cleanup, the histograms are marked "≈ / Approximate until rescan".
- Optional accessibility patterns by type: **dots** (stripes remain reserved for "incomplete").

### 2.10 Identity, accessibility and languages
- **Palette** (light / dark):
  - Background `#F5F6F8` / `#1B1D21`
  - Surface `#FFFFFF` / `#25282E`
  - Accent `#0F6CBD` / `#4CA3F0`
  - Safe `#107C41` / `#3FB871`
  - Review `#B26A00` / `#F2B24D`
  - Info only / danger `#C42B1C` / `#FF6B5E`
- **Fonts:** loaded from the system at runtime, without redistributing them: `SegUIVar.ttf` if it exists (measured: egui applies `wght` from 300 to 700; `opsz` is not applied on its own), otherwise `segoeui.ttf` + `segoeuib.ttf`. The path is obtained with `SHGetKnownFolderPath(FOLDERID_Fonts)`. **No egui fonts are embedded** (`default_fonts` disabled); non-Latin scripts use system fallbacks. If there were no usable system font at all, the app warns and exits.
- **Icons:** a subset of about 30 **Fluent UI System Icons** (MIT, © Microsoft, confirmed), embedded. They look the same on Win10 and Win11. It does not depend on `SegoeIcons.ttf`.
- **Solid background.** Mica does not work with wgpu (measured in 4 configurations). It would only come back if we switched to plan B `glow`.
- **Languages:** Spanish and English from day 1 (`fluent-bundle`), with no literal strings in the code.
- **Accessibility:**
  - PerMonitorV2 DPI.
  - 2 px visible focus and a treemap navigable with the arrow keys.
  - AccessKit in the list.
  - "Reduce animations" is respected (`SPI_GETCLIENTAREAANIMATION`), also in the indeterminate progress and the wait indicators.
  - AA contrast.

### 2.11 Persistence
- `%APPDATA%\Tesela\config.toml`: settings.
- `%LOCALAPPDATA%\Tesela\history.jsonl`: **summaries only** (date, drive, used, freed now, sent to the Recycle Bin). It can be deleted from Settings. The tree is never stored.
- Basket when relaunching as administrator: it is saved to a file in `%LOCALAPPDATA%` with a restricted ACL. The new process reads it, validates the paths again, recalculates the sizes and deletes it. If the elevation is with **another account**, a warning is shown and it is not recovered.

---

## 3. Architecture

### 3.1 Workspace
```
tesela/
├─ Cargo.toml              # [workspace], profiles, [workspace.dependencies]
├─ rust-toolchain.toml, deny.toml, LICENSE-MIT, LICENSE-APACHE
├─ crates/
│  ├─ model/    # flat records, TreeBuilder, Tree (CSR), overlay, histograms (no Windows deps)
│  ├─ scanner/  # parallel walker, priority queue, MFT (trait MftSource)
│  ├─ layout/   # squarified, "Others" grouping, LOD, hit-test (pure)
│  ├─ cleaner/  # TOML rules (risk + destination), protection, permanent deletion, Recycle Bin, duplicates
│  ├─ ui/       # egui: views, theme, assistant, i18n
│  └─ app/      # main, manifest, elevation, persistence
├─ tests/fixtures/  ├─ benches/  └─ .github/workflows/
```

### 3.2 Data flow: one `TreeBuilder` for both scanners
```
walker ─┐                         ┌─ aggregates bottom-up (alloc, logical, files, histograms)
        ├─► flat records ─►  TreeBuilder ─┤─ dedups hard links by FileId
MFT ────┘   (Batch per dir)       └─ sorts children by size ─► assigns final ids in BFS ─► Arc<Tree> (CSR)
```
- **Flat record:** `parent: u32`, `name_off: u32`, `name_len: u16`, `flags: u16`, `alloc: u64`, `logical: u64`, `mtime: u32` (32 B, fields reordered for alignment). The `FileId` (u128, because of ReFS) goes separately, in the `Batch`, and only lives during construction.
- **Order:** sorting happens **only once** in the builder, after aggregating. Since the final ids are assigned after sorting, `parent` does not need to be remapped.
- **Hard links:** they are **always** deduplicated by FileId across the whole NTFS/ReFS volume, in the builder, deterministically. A `Vec<(file_id, idx)>` is sorted. Within each collision group, the size is kept by **the link with the lexicographically smallest full path**; rebuilding paths is cheap because it is only done for the collisions. The flat index is not acceptable: it depends on how the threads share the work and would make the totals jump between runs. The other links keep `alloc = logical = 0` and the `HARDLINK` flag. The dedup is done **after the BFS**, only among reached entries (an orphan never beats a real link). Cost: +8 B per entry and a sort that is negligible next to the scan (measured). In phase 0, without dedup a small percentage of the volume and a considerable fraction of `C:\Windows` were over-counted. It does not apply on FAT/exFAT. Within a subfolder, only what has been scanned is deduplicated.

### 3.3 Walker
- Dedicated pools for scanning, deletion and hashing, so that they do not compete with each other.
- **Work-stealing** (`crossbeam-deque`), with one task per directory.
  - The worker enumerates the directory and obtains `base_id` with an `AtomicU32::fetch_add(n)`, so the subdirectories already have an id before being queued.
  - It sends the `Batch { base_id, entries, names }` to a collector thread that only does `push`.
  - No `unsafe` and no shared arena. It is one message per directory (~0.3 M per million files).
- **Priority queue:** an `Injector` that the workers check before stealing work. It holds `%TEMP%`, `Windows\Temp`, **specific browser cache directories (never the profile)**, `$Recycle.Bin\<SID>` and Downloads. The exact, conservative classification lives in `classify_browser_path` (`scanner/src/known.rs`) and the `cleaner` will reuse it; the cache names must be verified against the official Chromium and Firefox source code before anything is deleted.
  - Their ids are reserved in advance, **together with the chain of ancestors** (e.g. `C:\Users\<usuario>\AppData\Local`), created as `INCOMPLETE` nodes. That way "Stop and view" never shows orphans.
  - When the general traversal reaches one of those directories, it **adopts** the existing id instead of scanning it again. The comparison is made **by the directory's `FileId`** and not by the lowercased path, because NTFS allows case-sensitive folders.
- **Threads:** by default `min(16, 2 × cores)`; idle ones wait with increasing backoff. They are tuned by measuring real throughput; `IncursSeekPenalty` is only a hint. Phase 0 observation (a single test machine, one run per value): the improvement saturates early and is several times better than with 1 thread. HDD, USB and network are still unmeasured.
- **Reparse points:** they are not followed (size 0 and a flag).
- **Cloud folders (OneDrive and similar):** if they are **populated** they are walked (opened while letting the cloud filter act; measured in inc. 1: no network traffic or hydration). If they are **not populated** (`RECALL_ON_OPEN` / `RECALL_ON_DATA_ACCESS`) they are not walked: they count their `AllocationSize` and stay `CLOUD|INCOMPLETE`, to avoid triggering downloads.
- **Fallback mode (`FindFirstFileExW`):** no `FileId`, so there is no dedup and no priority queue; file `alloc` is estimated (EOF rounded to the cluster) and directory `alloc` is 0. Hard links are counted N times and the gap may be clamped to 0: the UI marks that scan as **"approximate"**.
- **Directory data from the parent's listing:** the `mtime` may lag by ~1 s right after writing (it is not usable for an incremental rescan) and the directory's own size comes out as 0 (it goes into the system data block).
- **Case:** the priority queue and the entry lookup compare exact names; case-insensitive comparison is only a fallback if the result is unique.
- **Long paths:** `\\?\` + `longPathAware` from the first commit. Names are stored in WTF-8.
- Cancellation with an `AtomicBool` that is created **before** the startup thread and checked between phases. Permission errors are counted without aborting. Errors are typed and the UI translates them with Fluent.
- The root goes through the same cloud and reparse classification as its descendants; its `FileIdInfo` is optional (if the volume does not support it, it continues with the fallback).
- **Live progress:** atomic counters plus atomic accumulators per first-level folder.
- **"Stop and view":** the builder works with the batches received and marks as `INCOMPLETE` the directories that still had pending tasks, and `INCOMPLETE_BELOW` on all their ancestors (for the UI's "≥"). If there are incomplete directories, the scan counts as stopped and the **unexplained space block is not computed**.
- A panic in a thread triggers cancellation (the coordinator does not hang); running out of ids cancels and is reported.

### 3.4 MFT (v0.3)
- `ntfs-reader::MftScan` in streaming mode → flat records → the same `TreeBuilder`.
- One MFT record is one node. DOS-only names are ignored.
- The `allocated_size` of all streams is added up, ADS included. Resident files count 0 clusters.
- `$BadClus:$Bad` is excluded. The mtime comes from `$STANDARD_INFORMATION`.
- On ReFS or with BitLocker locked, it falls back to the walker.
- Option to scan over a VSS snapshot.

### 3.5 Final model
```rust
#[repr(C)]
struct Node {            // 40 B; contiguous children sorted by size (guaranteed by the TreeBuilder)
    alloc: u64, logical: u64,
    parent: u32, first_child: u32, child_count: u32,
    name_off: u32, name_len: u16, flags: u16,   // DIR, REPARSE, CLOUD, HARDLINK, INCOMPLETE, INCOMPLETE_BELOW, SYSTEM_INFO, ERROR…
    files_below: u32,
}
// Side columns (all nodes, indexed by id; the BFS ids mix folders and files):
//   mtime: Vec<u32>
//   hist:  Vec<Hist { type_pct: [u8; 7], age_pct: [u8; 5], dominant_type: u8, dominant_age: u8 }>  (14 B)
```
- The thresholds are computed **on the `u64` accumulators** (with saturating addition) in the builder and stored in `dominant_type` / `dominant_age`. Dominant type: strictly > 50 % of the bytes. Age: the oldest band such that ≥ 80 % of the bytes are in it or in an older one; a "recent" result can also mean "mixed". An unknown `mtime` (0) does not count toward age. Weights = `alloc` after the dedup. The `u8` values are only used for drawing and for the tooltip.
- The extension → type table is an embedded TOML.
- **RAM**, an estimate to be validated in phase 0:
  - In steady state: about 78 B per entry (40 for the node, 4 for mtime, 14 for histograms and the name), measured in inc. 1; more with long names.
  - Peak during construction: **≈ 2× the steady state** (flat records + temporaries + final tree coexist); it depends on name length.
  - The UI adds about 65–70 MiB of private working set (measured with wgpu DX12 and the map drawn).
  - **Memory metric:** always **private working set** (the "Memory" column of Task Manager). The total working set includes shared driver DLLs and gives 82–174 MiB with any backend.

### 3.6 Scan ↔ UI and mutation after cleaning
- The UI reads the atomic counters at 15–30 Hz.
- When it finishes, an immutable `Arc<Tree>` is published with `arc-swap`. The UI takes no locks.
- `crossbeam-channel` carries the events.
- **Cleaning overlay** (`Arc::make_mut` is not used, as it would copy more than 64 MB):
  - A bitset of deleted nodes (125 KB per million).
  - An `FxHashMap<u32, Delta>` for the ancestors: when deleting, the delta is subtracted along the ~30 ancestors at most.
  - Visible size = base + delta. The layout reorders the visible children.
  - It is compacted on rescan.
- **Undo (v0.2):** `trash::os_limited::list/restore_all` (they restore correctly, measured in phase 0). It is matched by `original_path` and time. It reverts the delta. It is only offered for what went to the Recycle Bin. `list()` is linear in the size of the **WHOLE Recycle Bin** (tens of seconds with 100k items), and `delete_all` can return an error having moved almost everything. That is why the result is **verified file by file**, and Undo is offered for small batches or whole folders.

### 3.7 Safe deletion (increment 3)
Principle: **verify and act on the same handle**, never "check by path and delete by path" (this closes the TOCTOU race pointed out by the external audit).

1. **Approval is an identity.** When the basket is confirmed, the `cleaner` reopens each item with `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS` and stores `(volume serial number, 128-bit FileId)` (`FileIdInfo`) and `NumberOfLinks` (`FileStandardInfo`). Paths are only used to find the objects again.
2. **Permanent deletion by handle.** It is opened with `DELETE` access and **`dwShareMode = 0`**. The exclusive open is at once the "in use" check (`SHARING_VIOLATION` → skipped and counted), it closes the `FILE_SHARE_DELETE` case (nobody else has it open) and it prevents substitution between checking and acting. If the handle's `FileIdInfo` does not match what was approved → it is rejected ("changed since the confirmation"). Then, `SetFileInformationByHandle(FileDispositionInfoEx, DELETE | POSIX_SEMANTICS | IGNORE_READONLY)` on **that same handle**.
3. **Ancestors and recursion.** Windows has no `openat`, but `NtCreateFile` with `OBJECT_ATTRIBUTES.RootDirectory = <parent handle>` is the relative open. The category root (`%TEMP%`, the cache directory…) is opened by handle, its FileId is verified against the captured one, and the children are enumerated and opened **relative to that handle**, each with `FILE_OPEN_REPARSE_POINT`, rejecting any reparse point. This is the pattern with which `std::fs::remove_dir_all` was rewritten after CVE-2022-21658. **Trust boundary:** the ancestors above the category root belong to the user's own profile; changing them requires the user's own privileges, and the app never runs elevated in v0.1. **Root chain (v0.1.1):** a category root is only accepted after walking its path, as written and from the volume root, component by component with relative opens (`FILE_OPEN_REPARSE_POINT`, attributes only), with no reparse point at all and with the path resolved by the handle equal to the canonical one; the walk returns the identity of the last component and the approval, when opening the root to list it, requires the same identity and the same path (otherwise `ReparseAncestor`: an ancestor swapped for a junction between the two steps does not approve the wrong object). It is repeated on every approval; a discarded root is not a category: it is recorded in the plan, in the log (`rejected` note) and the interface shows it as "not available" with its reason. Residual: the ancestors of `%LOCALAPPDATA%` are resolved earlier (canonical known folders, the user's own profile).
4. **Recycle Bin (no tombstone).** Prior check with an exclusive open (identity and "in use"); then a shared **guard handle** `DELETE | READ_ATTRIBUTES` is opened and kept for the whole shell operation and until the result is judged (it blocks renaming its **ancestors**, error 5, without preventing the shell from recycling; it does **not** prevent renaming the item itself or putting another one in its place under its name, because it shares `DELETE`). It is recycled **by the original path** with `IFileOperation` (`FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_NO_CONNECTED_ELEMENTS`: without the last one, an `.html` page dragged its `_files` folder along) and an own `IFileOperationProgressSink`: in `PostDeleteItem`, `psiNewlyCreated` is the item already in the Recycle Bin (null if the shell deleted it permanently). `Recycled` requires: same identity (the `$R` FileId), the approved name no longer exists, the guard's final path is in `$Recycle.Bin` and the links did not change; otherwise `RecycledOther` (recoverable, reported and not applied to the tree). Null with success → "deleted permanently (it did not fit)". **Sink (v0.1.1):** `PreDeleteItem` opens the item by its path without following links and compares volume + FileId with what was approved for that exact path; a substitute, a child or any other path is vetoed with `E_ABORT` (the shell cancels what is pending). Residual, stated precisely: between that check and the move the shell acts by path, and a substitute placed exactly there reaches the shell; the only guarantee is that it is **detected** afterwards (if it fits and the shell gives its path, `RecycledOther`, recoverable; if it does not fit, the shell deletes it and it is recorded as `DeletedUnexpectedly`; if it gives no path or fails, `Unverified`). **v0.1.2:** "not sent" (`NotRecycled`) is only asserted with the approved object intact in its place and, for a directory, with the **fingerprint** of its contents identical (sum and XOR of a per-entry hash of FileId, sizes, links and class, independent of order); it also applies to shell **failure** results and to "no notice"; a partial loss is `DeletedUnexpectedly` and stops the sending. A directory measurement is only valid if it is **complete** (no `denied`, `in_use`, `other_errors`, `gone` or `changed`); what grew compared with what was approved or with the last measurement is not sent. **v0.1.3:** a **directory always travels alone** in its operation: the Recycle Bin occupancy is read, the directory is measured and `take_op` is applied with that measurement and that occupancy, and it is handed over with nothing in between except the log's intent line. The **contiguous files at the head of the queue** (the group is cut at the first directory) can travel together because `PreDeleteItem` measures each one again **through its guard handle** (`FileStandardInfo`, the greater of bytes on disk and logical size, with no tolerance) and, if it takes up more than what was counted for the budget, vetoes with `E_ABORT` ("changed since the confirmation"). Cost measured with the real shell and 500 small files: ≈ 7 s in one operation with the check (indistinguishable without it) versus ≈ 70 s with one operation per file; one directory per operation costs on the order of 0.15 s. Residuals of v0.1.3: what grows while the shell operation itself runs or between `PreDeleteItem` and the move; in a directory, between the walk and the handover; and the Recycle Bin occupancy, read before the walk. v0.1.4 closes the last two with change monitoring and the double reading of the occupancy (below), except for the NTFS limit with open writers. The content check of a directory that was not sent **runs first and decides**, with three states: intact (complete and equal measurement), changed (complete and different measurement: `DeletedUnexpectedly`, which states the **difference** —"deleted permanently, or its contents changed or lost a part"—, not a certain deletion) and unknown (incomplete, failed or cancelled measurement: `Unverified`, without asserting a loss); changed and unknown stop the sending. **A failure result (`hrDelete < 0`) when sending cuts the batch** (the sink returns `E_ABORT`); the purge does not change. `ShellDeclined` only occurs with the contents intact and the pending items cancelled; a failure of one file with pending items in its batch stops the whole sending (conservative). The sink's own `E_ABORT` is not recorded as a Windows error. A folder with a Recycle Bin destination that already could not be read in full at approval is rejected ("could not be read in full: N items without access"). `PreDeleteItem`/`PostDeleteItem` receive the **long** path (`SIGDN_DESKTOPABSOLUTEPARSING`): every comparison is made in canonical long form (in CI `%TEMP%` is 8.3). `PostDeleteItem` records the event and returns `E_ABORT` if the shell deleted permanently or gave no path: the cut no longer waits for the end of the batch, and `Unverified` also cuts. The judgment of "deleted" is **by object and by content, not by name**: with the guard open, a deleted file remains visible as pending deletion (`DeletePending`; measured `hr = 0x00270008`), so it is only "the shell touched nothing" (`ShellDeclined`, measured `hr = 0x00270003` with a folder that contains a huge sparse file) if the guard is not pending deletion, reopening the path gives the same identity and, for a directory, the measurement (size and number of entries) is identical to the one taken right before sending; any other case is a permanent deletion, even if partial. The sink's logic is pure code (`SinkCore`, `post_delete_aborts`) tested without COM. **Split into operations** (`take_op`): each operation ≤ 90 % of `MaxCapacity` minus the current occupancy (`SHQueryRecycleBin`); what is large (> 10 %) goes alone; without known capacity or occupancy, `NoRoom` (fails closed); each directory is measured again right before the operation that includes it (if it grew it is not sent, "changed since the confirmation"); what does not fit stays `NoRoom` without reaching the shell; **safety cutoff** after the first unexpected deletion (the rest stays where it was). Without `MaxCapacity` there is no capacity to guarantee (`Unfit(CapacityUnknown)`: only with explicit confirmation of permanent deletion, with the hint to set a maximum size in the Recycle Bin properties); the `NoRecycleFiles` policy and any doubt = "does not fit" (fails closed). The renamed tombstone of the previous design was withdrawn because the user saw and restored the tombstone name and orphans could be left behind. Measured: `FOF_WANTNUKEWARNING` with `FOF_NO_UI` hangs (its test runs in a child process with a timeout).
   - **Change monitoring (v0.1.4).** A directory carries a watch (`ReadDirectoryChangesW` over the subtree, on a second handle with verified identity) opened **before** the measuring walk and alive until its `PreDeleteItem`, where it is **stopped** after the last check. Any notification, overflow or error invalidates the measurement: before sending, it is not sent (checked before and after writing `recycle-begin`); in `PreDeleteItem`, veto with `E_ABORT`; in both cases "changed since the confirmation". A folder without a watch is not sent. NTFS limit: the size and last-write notifications of a file that another process keeps open arrive on flush or close; such a writer (with `FILE_SHARE_DELETE`) that grows it within the window can reach the shell with more than what was measured: the shell decides, if it does not fit it deletes, Tesela detects it (`DeletedUnexpectedly`) and stops the sending. Additions, removals and renames notify instantly. The residual window starts at `PreDeleteItem`. Cost ≈ 170 µs per directory (debug) versus ≈ 1.2 ms for the measuring walk. If the user puts `%LOCALAPPDATA%\Tesela` (or an ancestor) in the basket, the log itself falls inside the watched subtree and the sending is vetoed (fails closed). The structural solution (having the shell **ask** before deleting, with `FOF_WANTNUKEWARNING` and an owner window) is a v0.2 spike.
   - **Fingerprint (v0.1.4; diagnostic age extension in v0.1.5).** A directory is only sent with the **same** contents that were approved: identical fingerprint, counts, longest descendant path and size of the reparse points, with no further protected entries, reparse points or cloud files; any change, shrinking included, goes back to confirmation. Charge against the Recycle Bin: `max(bytes + shared + reparse; logical + reparse logical)`; the reparse points inside count their own size according to the listing (an unhydrated cloud placeholder counts its full logical size, on purpose) and their target is never measured. The v0.1.5 diagnostic rules are the deliberately reviewed exception with a Recycle Bin destination and `min_age = 168 h`: they expand by eligible children, recheck age through the handle during approval and right before sending, keep younger children, and never allow permanent deletion by category or by a path under their roots. At approval, a linked ancestor (junction, symlink, mount point, WSL link) is rejected as `ReparseAncestor`; cloud sync folders are tolerated as an ancestor (a loose file under OneDrive still reaches `confirm-cloud`); the children are opened relative to the listed directory with the listing's FileId and volume.
   - **Occupancy (v0.1.4).** The Recycle Bin occupancy is read before and after the walk and the worst one wins (greater occupancy, smaller capacity); it is a heuristic, not a guarantee. The 90 % is the same exact `u128` calculation in the fit check and in the split.
5. **Hard links.** The FileId is not needed in the model: if `NumberOfLinks > 1` at approval, the item is flagged in the confirmation ("shared with another file: it will not free space") and excluded from the estimate of freed bytes. The approved `NumberOfLinks` is stored exactly: if it changes at execution time, it is rejected ("changed since the confirmation").
6. **Emptying the Recycle Bin:** enumerated scope; it is purged by the items of the Recycle Bin's shell folder (`DeleteItem` with the same sink). In this Windows build the shell leaves the `$I` orphaned, so `remove_orphan_meta` deletes the exact sibling `$I` of a just-purged item (only under the user's own SID folder, with the `$R` already gone, valid `$I` contents that match the item, exclusive open and read/delete through the same handle). The `delete` verb of the Recycle Bin's context menu hangs and is not used. **v0.1.1:** when enumerating, the identities of the `$R` and the `$I` are captured and checked again when purging (if they do not match, the item is skipped); the orphaned `$I` is only deleted if the `$R` is *not found* (any other error is "unknown") and name, original folder, size and deletion time match; without identity (FAT/exFAT) it is not purged and is counted separately. Purge with `FOF_NO_UI | FOF_NO_CONNECTED_ELEMENTS`. **v0.1.2:** each chunk of 500 opens a guard per `$R` (verified by identity) and the operation carries a mandatory sink **per type** (`Operation::new` does not accept `None`) whose `PreDeleteItem` checks again by path the `$R` and the `$I` (the children of a Recycle Bin folder, by the identity of their `$R`) and vetoes with `E_ABORT`. Measured with the real shell: the vetoed item receives `E_ABORT` (`0x80004004`) and its `$R` remains; with the veto in the middle of a batch, the previous one is purged and the next one receives no notice; a veto also cuts the remaining chunks. The veto is attributed to the list item (`Veto::id()`) and recorded as "changed". At the end, the guard shows whether what was deleted was the approved object (`delete_pending`); if not, "replaced". Residual: between the check by path and the actual deletion; a substitute placed there is deleted (it is detected right afterwards).
7. **Three counters.** "Deleted permanently" and "moved to the Recycle Bin" are sums of sizes; **"Freed now" is the measured difference of `GetDiskFreeSpaceEx` before and after** (it automatically includes hard links, open handles and unexpected permanent deletions). If they diverge, both are shown with the reason in one line.
8. **Browser.** Detecting the process is UX ("close Chrome"); the guarantee is point 2: if the browser starts halfway through, the remaining files fail the exclusive open and are skipped.
9. **Parallelism and progress.** Files in a dedicated pool and directories afterwards, bottom-up; progress with atomics and cancellation between entries. `SHARING_VIOLATION` and `ACCESS_DENIED` are skipped and counted.
10. **Simulation mode (`--dry-run`)** that records the approved identities without touching anything, and a local log of what was deleted. Names `limpieza-<AAAAMMDD>-<HHMMSS>-<3-digit sequence>[-simulacion].jsonl` (`limpieza` = cleanup, `simulacion` = simulation; the file names are literal); retention is per class (30 real cleanups, 10 simulations), sorts by date, time and sequence as numbers (also the v0.1.1 names), never deletes the log it has just opened and checks that it exists after creating it.
11. **Mandatory `cleaner` tests** on trees in `%TEMP%`, which **compete with the operation**: replacing a file with another of the same size, swapping an ancestor for a junction, opening a file exclusively from another thread… and checking for rejection or skip. The shell sink's decisions are also tested without COM (CI has no desktop Recycle Bin); with `TESELA_REAL_RECYCLE_TESTS=1` they are confirmed with the real shell and own items. The sink's COM callbacks are also tested directly (without a shell operation). The leftovers of those tests are cleaned by a safety net that only touches items from a test (`tesela-test-<uuid>`, UUID form required, deleted more than 2 minutes ago), deletes them by path only under the own Recycle Bin and without reparse points, fails closed and fails the test if anything is left. Every path comparison is made on long canonical forms (in CI `%TEMP%` is 8.3).
12. **Local drives only:** the final resolved path must be on a local drive with a letter (fixed or removable); UNC paths (including the machine's own administrative share) and network drives are rejected (`NotLocal`) already in the basket. `TEMP` is only accepted if it is `%LOCALAPPDATA%\Temp` or hangs from it, and an exemption never lifts a denial below it; since v0.1.1 each exemption only lifts the **single denied zone it declares** (its `User Data`, `Mozilla\Firefox`, the Windows folder; `%TEMP%` none), never "that zone or any above it".
13. **Window closing blocked** while a cleanup runs (from any screen); local log with `start` and `recycle-begin` flushed before each operation so that what was moved can be reconstructed after a forced close. **v0.1.1:** without a usable log there is no destructive action at all (the simulation may run without it); a write error cancels the cleanup at the next safe point; before each operation an intent line is written and flushed (`delete-begin`, `recycle-begin`, `bin-begin`) and emptying the Recycle Bin records each item (`bin-item`); `flush` after each line and `sync_all` on creation and on finishing (the modeled risk is the forced close). The report says whether the log was truncated and distinguishes the user's cancellation from the log failure. A log is only discarded if no operation was written. `open_rel` rejects names with `:` (alternate streams). **v0.1.2:** the result of each entry is written and flushed to the system as soon as it is produced, with its error checked; the flush is **to the system, not to disk** (`sync_all` only on creation and on finishing). After a forced close the only thing that can be missing is what was in flight: one entry per worker thread (up to 32) in permanent deletion, or the whole batch of a shell operation (its list is recorded in its intent). Measured cost: 6 to 13 µs per result in the writer; deleting 100,000 own files, +6 % with 1 thread and +2 % with 4. Cap per log: 250,000 lines or 64 MiB (past the cap, `log_truncated`). **v0.1.3:** the open log is protected: it is opened with `FILE_SHARE_READ | FILE_SHARE_WRITE`, without `FILE_SHARE_DELETE`, so while a writer has it open nobody can delete or rename it (reading it is allowed); the guarantee holds among writers ≥ 0.1.3. Pruning skips the open log (retention may exceed the limit while a writer is alive); `discard` closes before deleting and leaves the writer unusable; "Delete the history" does not delete the open log.
14. **Reparse points:** measured with junctions: relative deletion never enters them. Directory symlinks remain to be measured (§6).

### 3.8 Treemap
- Squarified algorithm by Bruls et al., linear because the children already arrive sorted.
- Depth-2 layout (one labeled level and one hinted level).
- Cache keyed by `(root, viewport, depth, overlay version)`. Hover is painted as an overlay and hit detection uses a spatial grid.
- **Cell budget:** `max_rects` is a single value for both levels and is applied before the first squarify (the excess goes into a suffix "Others").
- **Drawing:** the layout is tessellated once into a `Mesh` that is reused as long as the cache key does not change. Target: **p99 of CPU per frame < 8 ms**. Measured: with `Mesh`, ~1–2 ms per frame; with one `RectShape` per cell it is not met even with 15k cells.

### 3.9 Manifest and elevation
- Manifest with `embed-manifest`: `asInvoker`, `longPathAware`, PerMonitorV2 and Win10/11 `supportedOS`. **No** `activeCodePage UTF-8`, because W APIs are used.
- Relaunch with `ShellExecuteExW("runas", "--scan C:\\")` only if it is the same user (see §2.11).
- In v1.0, an elevated `--mft-helper` helper that returns the compact result over a named pipe with an ACL limited to the user's SID. That way the UI stays unprivileged.
- **The application does not clean while elevated (v0.1.2).** The manifest is `asInvoker`, but an elevated process that launches Tesela passes its privileges on to it: the `cleaner` queries `TokenElevation` and, if it is elevated or cannot be queried (fails closed), `execute` returns `ExecError::Elevated` before touching anything and emptying the Recycle Bin is not done; analysis and simulation remain allowed. The functions that delete are `pub(crate)`: they cannot be bypassed from another crate. Known limit: with UAC disabled or with the built-in Administrator account every process is elevated and Tesela does not clean (the README and `SECURITY.md` explain it).
- **DLL loading (v0.1.2; v0.1.4 adds `SetDllDirectoryW("")`, which removes the current directory from the search).** `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` as the first statement of `main` (in `tesela` and `tesela-scan`) and `/DEPENDENTLOADFLAG:0x800` in the link step (static imports that are not KnownDLLs, such as `dxgi`, `dwmapi`, `uxtheme`, `propsys`, `uiautomationcore`, are resolved before `main`). The DX12 shader compiler is pinned to FXC (`dxcompiler.dll` is not probed). Tested: with files named like DLLs next to a copy of the exe, it starts, paints and loads none of them; COM/Shell, UIA and the GPU driver keep working.

### 3.10 Dependencies (versions of 2026-10-15; **verify them again when writing `Cargo.toml`**)
`eframe/egui 0.36.2` · `windows 0.62.2` / `windows-sys 0.61.2` (with `windows-core` 0.100 separately; tolerate duplicates in `cargo-deny`) · `crossbeam-deque 0.8.8` · `crossbeam-channel 0.5.17` · `arc-swap 1.9.2` · `rayon 1.12` · `trash 5.2.9` · `xxhash-rust 0.8.19` · `blake3 1.8.7` · `rustc-hash 2.1.3` · `thiserror 2.0.21` · `dunce 1.0.5` · `embed-manifest 1.5.1` · `rfd 0.17.2` · `serde` + `toml` · `fluent-bundle` · `criterion 0.8.2` · (v0.3) `ntfs-reader 0.6.0` · (v1.0) `usn-journal-rs 0.5.0`.

### 3.11 Quality and distribution
- **CI** on `windows-latest` (actions pinned by SHA, `timeout-minutes`, Dependabot): `fmt --check`, `clippy -D warnings`, `cargo test`, `cargo deny` (also weekly), MSRV 1.95, ARM64 `cargo check`, release build and a **privacy job** (`scripts/privacy-check.ps1 -All -GenericOnly` and its tests). Locally, `pre-commit` and `commit-msg` hooks with the same check and local values.
- **Every published binary is built with `scripts/build-release.ps1`** (`--remap-path-prefix` and **static CRT**, `+crt-static` in `.cargo/config.toml`), which requires the exact set of executables (Cargo's JSON output), verifies that they contain no paths or machine names and, with `scripts/check-pe.py`, that the import table does not depend on the C runtime (`VCRUNTIME*`, `MSVCP*`, `api-ms-win-crt-*`).
- CI also reviews the message and **all the blobs of every commit** in the push or PR (`privacy-history` job); `privacy-check` rejects opaque or compressed formats by signature, and UNC paths. The `--json` and `--screenshot` tools do not overwrite files without `--overwrite`.
- "Open in Explorer" uses `SHParseDisplayName` with exact UTF-16 + `SHOpenFolderAndSelectItems`; no path goes through `str`/`display()` (a rule for the `cleaner` too).
- **Tests:**
  - The builder (aggregation, ordering, hard link dedup, histograms and thresholds), the layout and the rules, all pure.
  - Seeded synthetic trees.
  - **`cleaner` tests on temporary folders**: it never deletes anything outside the allowed set, absorbs descendants and passes the TOCTOU revalidation.
  - Differential MFT/walker test on a VHDX created in CI. They must match except for the NTFS metadata and the denied folders.
  - Fuzzing of the MFT parser before 1.0.
- **Release:** `lto = "fat"`, `codegen-units = 1`, `strip`. `panic = "unwind"` in 0.x and `abort` only after fuzzing.
- **Signing:** SignPath Foundation (OSI license, automated build, signing policy published in the README).
- **Delivery:** **per-user MSI from v0.1**, with **WiX 5.0.2** (`wix` CLI as a `dotnet tool`, version and SHA-512 pinned; hand-written `.wxs`, with no custom actions; from WiX 6 the package requires accepting the OSMF agreement), plus a portable zip and `SHA256SUMS.txt`. `scripts/build-msi.ps1` always starts from `build-release.ps1` (reproducible binaries with `/Brepro`) and validates the MSI with `scripts/msi-validate.ps1` (tables on a closed list). `release.yml` (`v*` tag): jobs `build` → `attest` (provenance) → `verify` → `release` as a **draft**; a `workflow_dispatch` rehearsal (`dry_run`) only from `main`. v0.1 ships **unsigned**; SignPath Foundation (free for open source) when the maintainer requests it (`CODE_SIGNING.md`). winget and published ARM64, in v1.0.

---

## 4. Roadmap

> **In force since 2026-10-16: [ROADMAP.md](ROADMAP.md)** (product thesis, segment, rules, catalog, 3-month validation phase and its decision metric). The table below stays as a record of the original design; whatever comes after v0.1 is decided by ROADMAP.md.

Design reference: an SSD with a volume of ~1 M files. Memory is always measured as **private working set** (§3.5).

| Phase | Content | Measurable milestone |
|---|---|---|
| **0. Spike ✅** | Done: see [phase-0-findings.md](phase-0-findings.md). wgpu DX12 backend + `Mesh`; `FileIdExtdDirectoryInfo` API; POSIX ruled out; Mica ruled out; destination for development folders deferred to v0.2 | Closed (round 4, §9) |
| **1. MVP v0.1** | ✅ Inc. 1 (model + scanner + `tesela-scan` CLI + CI). ✅ Inc. 2 (layout + read-only interface + `--demo`). ✅ Inc. 3 (safe deletion, §3.7). ✅ Inc. 4 (per-user MSI + draft release). Walker + priority queue + TreeBuilder (with hard link dedup), home, scan screen with verdict, treemap (cached `Mesh`) + list + details, system block, basket by destination, permanent deletion + Recycle Bin, protection, Temp/caches/Recycle Bin/pagefile/hiberfil, summary, summary history, es/en, light/dark. **Deliverable: per-user MSI installer** + portable zip, generated in CI | 1 M files in < 30 s warm; known-paths verdict in < 5 s; private working set ≤ 75 MiB idle with the map drawn and ~85 B per entry in steady state (≈ 155 MiB with 1 M; accepted by the user); p99 of CPU per frame < 8 ms with hover; **0 deletions outside the allowed set** in the tests; the MSI installs, updates and uninstalls without administrator |
| **2. v0.2** | Development folders (with the destination decision and "rename first", §2.6), old downloads and installers, large files, color by age and risk, filters, Undo + `Ctrl+Z`, visible history, shortcuts, arrow keys in the treemap | Relayout of 20k rectangles < 5 ms |
| **3. v0.3** | MFT + "Restart as administrator" (with the persisted basket) + differential test | 1 M < 3 s; 10 M < 30 s; totals equal to the walker except for NTFS metadata and denied folders |
| **4. v1.0** | Duplicates, WinSxS / restore points, elevated helper, incremental USN, Exclude + persistent exclusions, CSV/HTML export, Explorer context menu (HKCU), winget, published ARM64, fuzzing, screen reader audit | Incremental rescan < 2 s; exe ≲ 16 MB (wgpu DX12 already takes ≈ 10.5 MB); signed packages |

---

## 5. Risks
| Risk | Mitigation |
|---|---|
| Deleting something improperly (especially permanently) | Permanent only with rules; `cleaner` as the defense; revalidation right before deleting; no following reparse points; tests on temporary folders; simulation and log |
| The Recycle Bin does not fit, or the drive is a network or removable one | Real limit recalculated at execution time and an explicit checkbox |
| OneDrive: deleting a synced file removes it from the cloud | Warning in the confirmation; cloud-only files cannot be selected |
| Over-counted hard links | Dedup by FileId in the builder |
| The UI does not start over RDP or in VMs without a GPU | `wgpu` DX12 with WARP by default; `glow` as plan B. Pending the user's test (§6) |
| Files open with `FILE_SHARE_DELETE`: the deletion seems to go well but frees no space | Detect the browser process; "rename first" in v0.2; count only what is really freed |
| Odd cases in the MFT parser; `ntfs-reader` is young | Trait, differential test, fuzzing, own fallback parser |
| Figures that are later not met (browser open, files in use) | "at least", "about", "up to"; the summary gives the real figure |
| SmartScreen or antivirus | SignPath |
| Treemap accessibility | The equivalent list with AccessKit |
| Elevating with another account | Warning, the basket is not recovered; helper in v1.0 |

---

## 6. Checks (phase 0)
Done (details in [phase-0-findings.md](phase-0-findings.md)):
- [x] `FILE_ID_EXTD_DIR_INFO` on Win11: it works and is the main API.
- [x] NTFS metadata files (`$MFT`…) do not show up when enumerating. `FSCTL_GET_NTFS_VOLUME_DATA` works without administrator with a handle to the root directory.
- [x] Memory, exe size and ms/frame of egui-`wgpu` versus `glow`.
- [x] 100k files to the Recycle Bin: ~2× the threshold (a single measurement); decision deferred to v0.2.
- [x] `restore_all` correct; `list()` is linear in the size of the whole Recycle Bin.
- [x] `FileDispositionInfoEx` + POSIX works, but is not faster: ruled out.
- [x] Reparse points (junction): deletion removes the link, not the target.
- [x] Mica (only with glow), `SegUIVar.ttf` (`wght` from 300 to 700), Fluent license (MIT), crate versions.
- [x] Name "Tesela": free on crates.io and winget (2026-10-15).

Pending (the first three are done by the user):
- [ ] **RDP and Hyper-V without a GPU** with the wgpu DX12 build (and glow as a comparison). Instructions in phase-0-findings.md (Pending).
- [ ] **Windows 10 22H2:** `FILE_ID_EXTD_DIR_INFO` and `SegUIVar.ttf`.
- [ ] **Directory symlinks** when deleting. It does not need administrator if Developer Mode is enabled.
- [ ] `FOF_WANTNUKEWARNING` together with `FOF_NO_UI` and the real Recycle Bin limit applied: **mandatory test of increment 3** (§3.7).
- [x] Increment 4 (MSI): static CRT without `VCRUNTIME140.dll` (verified in the imports); install/update/block older version/repair/custom folder/uninstall cycle without elevation, with the machine identical at the end; package validation (executables, notices, no local paths).
- [ ] MSI still to be tested: two users, application open during the update (Restart Manager), rollback of a failed installation, EXE/MSI signatures.
- [x] WiX 5.0.2 on GitHub Actions (no `cargo-wix`). Pending: `winget-releaser` and ARM64 runner (v1.0).
- [ ] **Repo configuration (done by the maintainer):** private vulnerability reporting, `v*` tag ruleset, `main` protection (PR + code owners + checks), `release` environment with a reviewer, "Require SHA pinning", immutable releases and MFA. Detailed list in `installer/README.md`.
- [ ] Registered trademarks for the name "Tesela" not reviewed.

## 7. Next step
With **v0.1.4** published (fourth round of fixes and change monitoring in the Recycle Bin sending), start the **3-month validation phase** of [ROADMAP.md](ROADMAP.md) §5: public security evidence, complete walkthroughs based on real cases and a pilot without the author's help. The rest of the roadmap depends on its decision metric. After the first stable version, the whole repository moves to English (the application stays in Spanish and English).

## 8. Out of scope (explicit)
Registry cleaner, "optimize" or defragment, uninstalling programs, network or cloud analysis in v1, accounts, telemetry or ads, sunburst or 3D, shredding or recovery, a full file editor or manager, automatic rules without user intervention.

## 9. Design review log
**Round 1** (review of the unified plan; 19 points plus 14 added by the agents):

| Problem | Resolution |
|---|---|
| "Always to the Recycle Bin" contradicted "free up space" | Risk and destination become separate axes; permanent only for what is regenerable by rule |
| CSR "sorted without sorting afterwards" was impossible | It is sorted once in the TreeBuilder and then the ids are assigned |
| Concurrent writes into the arena | Batches per directory + `fetch_add` of `base_id` + collector |
| MFT and walker built the model through different paths | Flat records → a single TreeBuilder |
| Hard links over-counted in the walker (WinSxS) | Dedup by FileId in the builder |
| The scanned sum ≠ the used space | "Windows and other system data" block |
| Immutable tree versus cleaning | Overlay with bitset + deltas |
| Verdict in 5 s versus a 30 s scan | Priority queue + "at least X GB" |
| Microsoft fonts and icons not redistributable / Win11 only | System fonts at runtime + Fluent UI System Icons |
| Protection was missing inside the profile itself | Untouchable as a target + "Contents of …" |
| Folder color by type and age undefined | 12 B histograms per directory, with 50 % and 80 % thresholds |
| Incoherent scope (Exclude, Undo, history) | Reordered by version (§4) |
| Browser open | Honest figures and "Check again" |
| Overlapping selections in the basket | Normalization by ancestors |
| `glow` on RDP | `wgpu` by default (hypothesis) |
| Slint in the spike | Removed |
| `activeCodePage UTF-8` | Removed |
| Principle 6 versus the "Review" categories | "Not preselected" |
| Synced OneDrive | Warning + not selectable if it is only in the cloud |

**Round 3** (validation by the advisor, Fable) — design validated with these corrections applied:
1. Cache of an open browser: it is not preselected and "Close Chrome to free up to X GB" is requested. POSIX deletion with `FILE_SHARE_DELETE` gave false "freed" figures and corrupted the cache index.
2. `%TEMP%` only deletes what is more than 7 days old (`min_age`).
3. "Empty the Recycle Bin" runs before moving items to the Recycle Bin.
4. Hard links: the one that keeps the size is the one with the smallest path in lexicographic order (deterministic), not the one with the smallest id.
5. Minor:
   - Histograms for all nodes (12 B).
   - `INCOMPLETE` ancestor chain in the priority queue and adoption by `FileId`.
   - `$Recycle.Bin`, `Default` and `Public` denied.
   - The headline adds up only what is Safe.
   - The MVP deletes with `remove_file` and leaves POSIX for v0.2.

**Round 14** (v0.1.5 beta, independent reviews of each feature followed by an integrated review):
- **Review diagnostics:** per-user WER reports and application crash dumps have fixed roots, `action = contents`, `min_age_hours = 168`, Recycle Bin destination and no preselection. A too-recent child is kept and recorded. The age is checked again from the open handle before sending. A category item, or a path under one of these roots added by hand, is never eligible for permanent deletion; an unfit item stays in place. More than 200 reports or dumps triggers a warning because each folder needs an individual shell operation. The whole `%LOCALAPPDATA%\Microsoft\Windows` directory is no longer a basket target.
- **Multiple selection before the basket:** Ctrl+click, Shift ranges (including shrinking a range), Space, Ctrl+A and Invert selection act on visible rows; marks clear when the folder changes. Search keeps only matching marks. The basket uses an index, `add_batch` and a revision counter, and its window virtualizes user rows; UI estimates are cached by basket revision and tree version. An actionable basket item still passes the ordinary basket protection checks.
- **Installer:** one dual-scope MSI with a native setup interface (personal or all users, folder, optional desktop shortcut, ES/EN). The package leaves the UAC-compatible bit clear so a personal install does not elevate and an all-users install asks for administrator permission; the all-users choice does not elevate the application itself. CI covers silent standard-user installation, upgrade and removal, and rejects an all-users attempt without credentials. Interactive UAC and another account's credentials still need manual Windows testing before publication.
- **Network:** an optional, off-by-default update check uses WinHTTP and the system proxy, at most once per 24 hours. It asks GitHub for the latest published release with fixed `User-Agent: Tesela`, no cookies, credentials, version or system identifier, and does not follow redirects. The validated version is used to build the release URL locally; no update is downloaded or installed. Demo and measurement tools do not check. Proxy authentication has not been tested in a real environment.
- **Cleanup responsiveness and mail data:** log creation and the completion history/process checks move to the worker thread; approval and execution show progress. An all-amber plan cannot run without the user's permanent-deletion consent. `.pst` (local Outlook mail) gets a separate acknowledgment before permanent deletion; `.ost`/`.nst` cache and an open Outlook instance get distinct warnings. Cancellation of an individual file deletion waits for that file operation to return. These warnings do not change the user's permission to delete their own files.

**Round 13** (external audits by Astra and Gemini on `v0.1.3`, verified by the coordinator; two Sonnet builds in parallel with disjoint scopes —cleaning and the rest—, independent Sonnet audits with correction of their findings, review with the advisor). Fixes in v0.1.4:
- **Sending to the Recycle Bin (fourth round of the same concurrency family):** change monitoring from before the walk until `PreDeleteItem`, identical fingerprint (shrinking also goes back to confirmation), occupancy read twice and charge including the reparse points inside (§3.7.4). The claim that the 90 % margin absorbed occupancy changes is corrected.
- **Approval:** a linked ancestor is rejected (`ReparseAncestor`, also under a volume mounted on a folder); OneDrive is tolerated as an ancestor (the first version rejected it and the audit caught it). Children opened relative to the listed directory. `C:` without a slash stops being a drive root (invalid key, denied).
- **Emptying the Recycle Bin:** a `$R` with links inside, or one that could not be fully checked, is excluded before being handed over ("not purged", `linked` log entry) without cutting the batch; the sink's veto remains as the last defense. An unreadable `$I` is skipped, recorded (`bin-unlisted`) and not included in what was promised. Events and vetoes are matched by exact name before `PathKey` (case-sensitive directories). Volumes mounted on folders are recognized in protection, the Recycle Bin and the drive type.
- **Scanner:** the priority queue validates each candidate component by component from the root (without following links, only `DirAction::Walk`, 8.3 names expanded, `..` discarded); a `\\?\Volume{GUID}\` root without a letter as a whole volume; `reparse_tag` only with `REPARSE_POINT`; no batch once the ids are exhausted; browser caches verified for the scanner and candidates for the `cleaner`. Accepted effect: a known folder under a populated cloud folder stops being a priority.
- **Model and UI:** `Overlay::add_up` subtracts from the ancestors what was really subtracted; saturating sums; WTF-8 rejects the 3-byte overlong encodings; stale measurements discarded after cleaning during a scan. Isolated session (`--idle-memory`, `--bench-hover`, with or without `--demo`) with no history and no real cleanup roots, and always a simulation if it is not the demo (M4 is only covered with unit tests: `SHGetKnownFolderPath` ignores environment variables).
- **Requested in the user's tests:** the sidebar drives look clickable (background, cursor, tooltip; the current one is not announced as a button) and there is a **Home** button with `Alt+Home` in the top bar, disabled during a cleanup. "Analyze a folder…" in the sidebar was dropped: it broke the legend at 1280×800; it is reached through Home.
- **Tools and installer:** `privacy-check` (size limit first, accumulated BOMs, UTF-8 without NUL, strings of 2+, author and committer with their label; when regenerating a screenshot, the previous one's hash goes into `$legacyDemoImages`); `check-pe.py` requires `DependentLoadFlags` 0x800 and REPRO; fixed shortcut GUID; new MSI validations. `INSTALLFOLDER` is **not** a secure property: the MSI has no interface and is per-user; marking it secure would only matter in an elevated installation, where it would let the user steer where the server writes. CODEOWNERS extended and `cargo deny --locked` with `all-features`. For whoever enables SignPath: sign the `.exe` files **before** packaging the MSI and the zip.
- **Incident and rule:** the machine's antivirus removed `cargo.exe` again while another auditor ran the tests that launch copies of the program with files named like DLLs next to them. Those tests only run with `TESELA_PROCESS_TESTS=1`, which CI defines; in CI a missing variable is a failure, not a skip. Locally, work continues with the previous toolchain until the user restores the current one; CI confirms with the pinned one.
- **Pending:** repeat `medicion_a_m3_*` (the log's figures are from v0.1.3, before `over_cap` took the writer's lock); `FOF_WANTNUKEWARNING` spike with an owner window (v0.2).
- **Single real pass** (real Recycle Bin, own items only, once and in series, on the merged tree): all 279 `cleaner` tests green, including the three new ones with the real shell (a three-level folder ends up `Recycled`, which confirms that the shell does not touch the tree before `PreDeleteItem`; a change right before `PreDeleteItem` is vetoed; an own `$R` with a junction inside is excluded and the rest is purged). The user's Recycle Bin intact.

**Round 12** (third external audit by Astra on `v0.1.2`, verified by the coordinator; Sonnet build, Sonnet audit with re-verification, review with the advisor). Fixes in v0.1.3:
- **A2-B1:** within a single operation the budget could become stale (an already measured folder grew while another was being measured). Now a directory always goes alone and the files of the batch are measured again through their guard in `PreDeleteItem` (§3.7.4).
- **A2-M1:** a shell failure after a partial loss let the rest of the batch continue; now a failure result cuts the batch.
- **A2-M2:** one writer's pruning could delete another's open log; now the open log does not allow shared deletion (§3.7.13).
- **A2-m1:** an impossible check was presented as a loss; now there are three states (intact, changed, unknown).
- The internal audit added negative tests for a directory's budget at handover and for the size check without tolerance, and texts that state the difference and not a certain deletion.
- **Incident:** a mutant campaign by the auditor ran the deletion test binary in a loop and the machine's antivirus flagged it as malware (it blocked 19 applications, removed `cargo.exe` and restarted the machine). New rule: on the developer's machine no mutants are run in a loop and no repeated tests with the real Recycle Bin; that goes to CI or a disposable VM. Associated product risk: an antivirus could distrust `tesela.exe` when cleaning (ROADMAP).

**Round 11** (external audits by Astra —deletion— and Grok —the rest— on `v0.1.1`, verified by the coordinator; two Sonnet builds in parallel with disjoint scopes, Sonnet audits with re-audit of each fix, review with the advisor). Fixes in v0.1.2:
- **Deletion (Astra):** A-B1 files are measured again through their guard right before each operation and the budget is rechecked; A-B2 a directory measurement is only valid if it is complete; A-M1 shell failures and "no notice" are judged as a possible deletion, with a content fingerprint; A-M4 emptying the Recycle Bin carries guards per `$R` and a sink that vetoes substitutes; A-M2 retention cannot delete the log just opened; A-M3 each result is flushed and checked; A-m1 the COM callbacks are tested directly. Also: no cleaning while elevated (E1), texts that promise neither figures nor absence of risk (T1), temporary files at 7 days (E2), honest `hiberfil` note (E3), the functions that delete are `pub(crate)` and the safety net of the real tests only touches items of the form `tesela-test-<uuid>`.
- **The rest (Grok):** G-m1 the scanner's root and priority paths do not follow a reparse point (link root → `ScanError::RootIsLink`); G-m2 `privacy-check` detects UNC paths with `/`; G-m3 the path rules apply in `spike/` (two historical files with synthetic paths allowed by exact SHA-256); G-S1 values split by NUL; G-S2 `install-wix.ps1` checks the SHA-512 **before** installing; G-S3 documented (there is no robust way to limit `INSTALLFOLDER` without a custom action).
- **DLL hardening** (found while investigating G-S3): `dxcompiler.dll` was probed by name and several static imports were searched first in the exe's folder (§3.9).
- Process incident: an auditor created junctions to a drive root and ran `rm -rf` on their folder (it deleted nothing; checked). New rule for all agents: never recursive deletion on folders with links, remove the links as links and do not create links to drive roots.
- Product: differentiation study and two external reviews → [ROADMAP.md](ROADMAP.md).

**Round 10** (external audit by Astra on `v0.1.0`, verified by the coordinator; Sonnet build, independent Sonnet audits with re-audit of each fix, review with the advisor). Fixes in v0.1.1:
- **B1:** a reparse point above a category root (`User Data`, `%TEMP%`) made protected content deletable, because the canonical root became an exemption and lifted any denial above it. Now the chain is walked by components without reparse points, the approval requires the same identity the chain saw and each exemption lifts only its declared zone (§3.7.3, §3.7.12). A discarded category is shown as "not available" with its reason.
- **B2:** the Recycle Bin split fails closed (without capacity or occupancy, `NoRoom`); the assumed 5 % is removed; directories are measured right before their operation.
- **M1, M2, S1:** `PreDeleteItem` vetoes substitutes; `PostDeleteItem` cuts the batch at the first permanent deletion or one without a path; `FOF_NO_CONNECTED_ELEMENTS`. The claim that the guard prevented renaming the item itself is corrected.
- **M3:** emptying the Recycle Bin checks the identity of `$R` and `$I`; the orphaned `$I` is only deleted with complete evidence.
- **M4, m2:** without a log there is no cleanup; intent before each operation; the simulation does not prune real logs.
- **m1:** exact `NumberOfLinks`. **m3:** `leak-scan` and `privacy-check` detect non-ASCII local values (UTF-8, Latin-1, UTF-16 without BOM) without losing UTF-16 detection. **S2:** `msi-validate` pins the folder tree, the Upgrade rows, the `FindRelatedProducts` < `LaunchConditions` order and compares case-sensitively.
- The internal audits added: the approval of a root compares the identity with the chain's; the judgment of "deleted" becomes by object and by content (a first version by name hid real deletions with the guard open, reproduced with the real shell); sink tests without COM; log retention by class; names with `:` rejected.
- **M5** (repository protections: rulesets, `main` protection, release environment, immutable releases) is maintainer configuration and stays on their list.
- New visible behavior: on a volume with no configured maximum Recycle Bin size, what would go to the Recycle Bin asks for explicit permanent deletion.

**Round 9** (increment 4, installer and release: Sonnet build, 2 Sonnet audits, review with the advisor):
- WiX 5.0.2 (not 6/7, so as not to accept the OSMF agreement on the user's behalf), per-user, no custom actions; `INSTALLFOLDER` persisted in HKCU so that updating and uninstalling respect a custom folder.
- Icon drawn by `build.rs` (the repo does not version images outside `docs/img/`) and embedded with `winresource`, with VERSIONINFO.
- Supply chain: actions by SHA, minimum permissions per job, attestation in an isolated job, verification before the draft, `--locked`, WiX with a clean `nuget.config` and checked SHA-512, reproducible binaries.
- Tests with the real Recycle Bin sit behind `TESELA_REAL_RECYCLE_TESTS=1` (GitHub runners have no desktop Recycle Bin); locally they are launched with the variable.
- The real security of publishing today is the draft that a person verifies: the repo configuration (rulesets, `main` protection, environment) is left to the maintainer.

**Round 8** (increment 3, deletion: Sonnet build, 3 Sonnet audits with attack probes, review with the advisor):
- The "verify and act on the same handle" design withstood all the races tried (substitution ×300, junctions in child/ancestor/root, hard links, renames, cancellation, killing the process).
- Blockers fixed: Recycle Bin verification by identity (not by name; it failed with `.lnk`/`.url` and with hidden extensions), local drives only (the machine's own UNC administrative share bypassed the protection), `TEMP` and exemptions (they could delete protected data).
- Design change: the tombstone is withdrawn (§3.7.4); Recycle Bin by original path with an event sink and guard handle; split into operations with a budget and safety cutoff (an operation with one item that did not fit permanently deleted the whole batch).
- Pending manual test before/after v0.1: `HideFileExt=1` (by construction it does not affect), a volume without `MaxCapacity`, the `NoRecycleFiles` policy, directory symlinks, mapped network drives. Deferred to v0.2: quadratic cost with trees thousands of levels deep and protection of system folders on other drives.

**Round 7** (external audit by Astra/ChatGPT on 6781a03, triaged with the advisor):
- **B1 (design):** deletion is redesigned as "approved identity + act on the same handle" (§3.7): exclusive open, FileId+volume, handle-relative recursion (CVE-2022-21658 pattern), Recycle Bin via a tombstone renamed by handle and per-item verification, Empty with enumerated scope, hard links detected by `NumberOfLinks`, "freed" = measured difference of free space.
- **M2:** browser caches are only specific cache directories; the profile never is (§2.6).
- **M1:** `--json` and `--screenshot` do not overwrite without `--overwrite`. **M3:** shortcuts respect the focused control. **M5/M6:** `privacy-check` reviews the whole message, rejects opaque formats and CI reviews all the commits of the push/PR. **m1–m11 and N1:** in the hardening cycle.
- **M4:** mentions of the measurement machine in the documentation are generalized (OS version and disk type). History is not rewritten: they do not identify the machine.
- **N2:** the unexplained space block is sorted by size; there are no egui fallback fonts.
- **Increment 4:** static CRT (`+crt-static`) and MSI/SignPath requirements added to §6.
- **Hardening cycle** (13 commits, Sonnet build + Sonnet audit + review with the advisor): M1–M6, m1–m11 and N1 fixed; exe ≈ 11.0 MiB with the static CRT. By user decision NO generic patterns for Windows version or disk capacity are added: what matters is personal information (folder and file names, user, profile paths, real screenshots).
- **Distribution:** user decision: **MSI only** (no MSIX or Microsoft Store). Signing with SignPath Foundation (free for open source) when the user requests it; v0.1 ships unsigned with the SmartScreen warning explained.

**Round 6** (increment 2 of the MVP: Sonnet build, 2 Sonnet audits, own verification and advisor):
- Memory: the target becomes ~85 B per entry in steady state (≈ 155 MiB with 1 M); **user decision**, it is not trimmed. Idle with the map: ~68 MiB.
- Fonts: without egui `default_fonts` (no OFL/UFL fonts are redistributed); SYSTEM fallbacks per script (CJK, Thai, Indic, Arabic, Hebrew, symbols), with kana and han always together. The font tests skip what the machine does not have (CI on Windows Server).
- Third-party notices: `THIRD-PARTY-NOTICES.md` generated with `cargo about` (not edited by hand), embedded in the `.exe` and visible in Settings › About.
- "Others": paginated when no child fits, so that opening it always advances. 4 px corners only on cells ≥ 16 px. Relayout of 20k cells ≈ 1 ms (a v0.2 milestone already met).
- Screenshots: `--screenshot` **requires `--demo`** (in code); `docs/img/` only admits PNGs registered with their SHA-256 in `docs/img/README.md`, with bounded fragments and lengths.
- Hooks without `-ExecutionPolicy Bypass` (antivirus heuristic).
- egui 0.36.2 is kept (0.37 exists; upgrading is a v0.2 task). While there is no risk (inc. 3), the scan header says "X GB in known places". Table of known paths with `KnownKind`.
- Pending for v0.2: depth of nested "Others" in very heavy tails (R4) and validating the `IDAT` zlib and the data URIs in `privacy-check` (R6). The measurement tools (`--bench-hover`, `--idle-memory`) may fail if the window is covered: that is the environment, not the app.

**Round 5** (increment 1 of the MVP: Sonnet build, 2 Sonnet audits, review with the advisor):
- 14 B `Hist` with `dominant_type` / `dominant_age`; 80 % rule clarified; `mtime` 0 excluded from age.
- Memory: steady state ~78 B per entry; peak ≈ 2× the steady state.
- 32 B `Record` with the `FileId` separate; dedup after the BFS and also with `logical = 0`.
- `FindFirstFileExW` fallback: no dedup or priority queue, "approximate" scan.
- `INCOMPLETE_BELOW`; no unexplained space block if there are incomplete directories.
- Cloud folders: populated ones are walked, unpopulated ones are counted without entering.
- Threads `min(16, 2 × cores)`; `cargo test` instead of nextest; published binaries only with `build-release.ps1`.
- Privacy: `privacy-check.ps1` with `pre-commit` and `commit-msg` hooks, local blacklist in `<git-common-dir>/privacy-denylist`, own tests and a CI job.

**Round 4** (phase 0 results: Sonnet build, Sonnet audit, final review with the advisor):
- **UI:** wgpu DX12 only, treemap as a cached `Mesh` (target p99 < 8 ms), solid background (no Mica).
- **Metrics:** memory = private working set (≤ 75 MiB idle with the map); exe ≲ 16 MB.
- **Deletion:** own POSIX ruled out; the `FILE_SHARE_DELETE` note is extended to `remove_file`/`DeleteFileW`; junctions measured.
- **Development folders:** destination deferred to v0.2, with "rename first".
- **Threads:** tuning by measurement continues (the improvement saturates early).
- **Undo:** verify file by file.
- **Deliverable:** per-user MSI from v0.1 (user decision).

**Round 2** (closing the cross decisions): development folders to the Recycle Bin by default (the phase 0 measurement may change it), histograms, pagefile/hiberfil as "info only" nodes, `wgpu` by default and basket persisted on elevation. Neither design nor architecture has blocking objections.
