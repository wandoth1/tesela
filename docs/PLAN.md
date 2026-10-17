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
   - **Fingerprint (v0.1.4).** A directory is only sent with the **same** contents that were approved: identical fingerprint, counts, longest descendant path and size of the reparse points, with no further protected entries, reparse points or cloud files; any change, shrinking included, goes back to confirmation. Charge against the Recycle Bin: `max(bytes + shared + reparse; logical + reparse logical)`; the reparse points inside count their own size according to the listing (an unhydrated cloud placeholder counts its full logical size, on purpose) and their target is never measured. Restriction: no rule with a Recycle Bin destination may carry `min_age > 0` without reviewing this comparison. At approval, a linked ancestor (junction, symlink, mount point, WSL link) is rejected as `ReparseAncestor`; cloud sync folders are tolerated as an ancestor (a loose file under OneDrive still reaches `confirm-cloud`); the children are opened relative to the listed directory with the listing's FileId and volume.
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

## 4. Hoja de ruta

> **Vigente desde el 2026-10-16: [ROADMAP.md](ROADMAP.md)** (tesis de producto, segmento, reglas, catálogo, fase de validación de 3 meses y su métrica de decisión). La tabla de abajo queda como histórico del diseño original; lo posterior a la v0.1 lo decide ROADMAP.md.

Referencia de diseño: un SSD con un volumen de ~1 M de archivos. La memoria se mide siempre como **private working set** (§3.5).

| Fase | Contenido | Hito medible |
|---|---|---|
| **0. Spike ✅** | Hecho: ver [fase-0-conclusiones.md](fase-0-conclusiones.md). Backend wgpu DX12 + `Mesh`; API `FileIdExtdDirectoryInfo`; POSIX descartado; Mica descartado; destino de las carpetas de desarrollo diferido a la v0.2 | Cerrado (ronda 4, §9) |
| **1. MVP v0.1** | ✅ Inc. 1 (modelo + escáner + CLI `tesela-scan` + CI). ✅ Inc. 2 (layout + interfaz de solo lectura + `--demo`). ✅ Inc. 3 (borrado seguro, §3.7). ✅ Inc. 4 (MSI por usuario + release en borrador). Walker + cola prioritaria + TreeBuilder (con dedup de hard links), inicio, pantalla de escaneo con veredicto, treemap (`Mesh` cacheado) + lista + detalles, bloque del sistema, cesta por destino, borrado definitivo + Papelera, protección, Temp/cachés/Papelera/pagefile/hiberfil, resumen, historial de resúmenes, es/en, claro/oscuro. **Entregable: instalador MSI por usuario** + zip portable, generados en CI | 1 M de archivos en < 30 s en caliente; veredicto de rutas conocidas en < 5 s; private working set ≤ 75 MiB en reposo con el mapa dibujado y ~85 B por entrada en régimen (≈ 155 MiB con 1 M; aceptado por el usuario); p99 de CPU por frame < 8 ms con hover; **0 borrados fuera del conjunto permitido** en los tests; el MSI instala, actualiza y desinstala sin administrador |
| **2. v0.2** | Carpetas de desarrollo (con la decisión de destino y "renombrar primero", §2.6), descargas e instaladores viejos, archivos grandes, colorear por antigüedad y riesgo, filtros, Deshacer + `Ctrl+Z`, historial visible, atajos, flechas en el treemap | Relayout de 20k rectángulos < 5 ms |
| **3. v0.3** | MFT + "Reiniciar como administrador" (con la cesta persistida) + test diferencial | 1 M < 3 s; 10 M < 30 s; totales iguales al walker salvo metadatos NTFS y carpetas denegadas |
| **4. v1.0** | Duplicados, WinSxS / puntos de restauración, helper elevado, USN incremental, Excluir + exclusiones persistentes, exportar CSV/HTML, menú contextual del Explorador (HKCU), winget, ARM64 publicado, fuzzing, auditoría con lector de pantalla | Reescaneo incremental < 2 s; exe ≲ 16 MB (wgpu DX12 ya ocupa ≈ 10,5 MB); paquetes firmados |

---

## 5. Riesgos
| Riesgo | Mitigación |
|---|---|
| Borrar algo indebido (sobre todo en definitivo) | Definitivo solo con reglas; `cleaner` como defensa; revalidación justo antes de borrar; sin seguir reparse points; tests sobre carpetas temporales; simulación y log |
| La Papelera no cabe, o la unidad es de red o extraíble | Límite real recalculado al ejecutar y casilla explícita |
| OneDrive: borrar un archivo sincronizado lo quita de la nube | Aviso en la confirmación; los archivos solo en la nube no se pueden seleccionar |
| Hard links sobrecontados | Dedup por FileId en el builder |
| La UI no arranca en RDP o en VMs sin GPU | `wgpu` DX12 con WARP por defecto; `glow` como plan B. Pendiente de la prueba del usuario (§6) |
| Archivos abiertos con `FILE_SHARE_DELETE`: el borrado parece salir bien pero no libera espacio | Detectar el proceso del navegador; "renombrar primero" en la v0.2; contar solo lo realmente liberado |
| Casos raros del parser MFT; `ntfs-reader` joven | Trait, test diferencial, fuzzing, parser propio de reserva |
| Cifras que luego no se cumplen (navegador abierto, archivos en uso) | "al menos", "unos", "hasta"; el resumen da la cifra real |
| SmartScreen o antivirus | SignPath |
| Accesibilidad del treemap | La lista equivalente con AccessKit |
| Elevar con otra cuenta | Aviso, la cesta no se recupera; helper en v1.0 |

---

## 6. Verificaciones (fase 0)
Hechas (detalle en [fase-0-conclusiones.md](fase-0-conclusiones.md)):
- [x] `FILE_ID_EXTD_DIR_INFO` en Win11: funciona y es la API principal.
- [x] Los metadatos NTFS (`$MFT`…) no aparecen al enumerar. `FSCTL_GET_NTFS_VOLUME_DATA` funciona sin administrador con un handle del directorio raíz.
- [x] Memoria, exe y ms/frame de egui-`wgpu` frente a `glow`.
- [x] 100k archivos a la Papelera: ~2× el umbral (una sola medición); decisión diferida a la v0.2.
- [x] `restore_all` correcto; `list()` es lineal en el tamaño de toda la Papelera.
- [x] `FileDispositionInfoEx` + POSIX funciona, pero no es más rápido: descartado.
- [x] Reparse points (junction): el borrado elimina el enlace, no el destino.
- [x] Mica (solo con glow), `SegUIVar.ttf` (`wght` de 300 a 700), licencia de Fluent (MIT), versiones de los crates.
- [x] Nombre "Tesela": libre en crates.io y winget (2026-10-15).

Pendientes (las tres primeras las hace el usuario):
- [ ] **RDP y Hyper-V sin GPU** con la build wgpu DX12 (y glow como comparación). Instrucciones en fase-0-conclusiones.md (Pendiente).
- [ ] **Windows 10 22H2:** `FILE_ID_EXTD_DIR_INFO` y `SegUIVar.ttf`.
- [ ] **Symlinks de directorio** al borrar. No necesita administrador si se activa el Modo de desarrollador.
- [ ] `FOF_WANTNUKEWARNING` junto con `FOF_NO_UI` y el límite real de la Papelera aplicado: **test obligatorio del incremento 3** (§3.7).
- [x] Incremento 4 (MSI): CRT estático sin `VCRUNTIME140.dll` (verificado en los imports); ciclo instalar/actualizar/bloquear versión anterior/reparar/carpeta personalizada/desinstalar sin elevación, con la máquina idéntica al final; validación del paquete (ejecutables, avisos, sin rutas locales).
- [ ] MSI pendiente de probar: dos usuarios, aplicación abierta durante la actualización (Restart Manager), rollback de una instalación fallida, firmas EXE/MSI.
- [x] WiX 5.0.2 en GitHub Actions (sin `cargo-wix`). Pendientes: `winget-releaser` y runner ARM64 (v1.0).
- [ ] **Configuración del repo (la hace el mantenedor):** reporte privado de vulnerabilidades, ruleset de etiquetas `v*`, protección de `main` (PR + code owners + checks), environment `release` con revisor, "Require SHA pinning", immutable releases y MFA. Lista detallada en `installer/README.md`.
- [ ] Marcas registradas del nombre "Tesela" sin revisar.

## 7. Siguiente paso
Con la **v0.1.4** publicada (cuarta ronda de correcciones y vigilancia de cambios en el envío a la Papelera), empezar la **fase de validación de 3 meses** de [ROADMAP.md](ROADMAP.md) §5: evidencia pública de seguridad, recorridos completos a partir de casos reales y un piloto sin ayuda del autor. El resto de la hoja de ruta depende de su métrica de decisión. Después de la primera versión estable, el repositorio pasa entero al inglés (la aplicación sigue en español e inglés).

## 8. Fuera del alcance (explícito)
Limpiador de registro, "optimizar" o desfragmentar, desinstalar programas, análisis de red o nube en v1, cuentas, telemetría o anuncios, sunburst o 3D, triturado o recuperación, editor o gestor de archivos completo, reglas automáticas sin intervención del usuario.

## 9. Registro de revisión del diseño
**Ronda 1** (revisión del plan unificado; 19 puntos más 14 que añadieron los agentes):

| Problema | Resolución |
|---|---|
| "Siempre a la Papelera" contradecía "liberar espacio" | Riesgo y destino pasan a ser ejes separados; definitivo solo para lo regenerable por regla |
| CSR "ordenado sin ordenar después" era imposible | Se ordena una vez en el TreeBuilder y luego se asignan los ids |
| Escritura concurrente en el arena | Batches por directorio + `fetch_add` de `base_id` + colector |
| MFT y walker construían el modelo por caminos distintos | Registros planos → un único TreeBuilder |
| Hard links sobrecontados en el walker (WinSxS) | Dedup por FileId en el builder |
| La suma escaneada ≠ el espacio usado | Bloque "Windows y otros datos del sistema" |
| Árbol inmutable frente a limpieza | Overlay con bitset + deltas |
| Veredicto en 5 s frente a escaneo de 30 s | Cola prioritaria + "al menos X GB" |
| Fuentes e iconos de Microsoft no redistribuibles / solo Win11 | Fuentes del sistema en tiempo de ejecución + Fluent UI System Icons |
| Faltaba proteger dentro del propio perfil | Intocables como destino + "Contenido de …" |
| Color de las carpetas por tipo y antigüedad sin definir | Histogramas de 12 B por directorio, con umbrales del 50 % y el 80 % |
| Alcance incoherente (Excluir, Deshacer, historial) | Reordenado por versión (§4) |
| Navegador abierto | Cifras honestas y "Volver a comprobar" |
| Selecciones solapadas en la cesta | Normalización por ancestros |
| `glow` en RDP | `wgpu` por defecto (hipótesis) |
| Slint en el spike | Eliminado |
| `activeCodePage UTF-8` | Eliminado |
| Principio 6 frente a las categorías "Revisar" | "No se preselecciona" |
| OneDrive sincronizado | Aviso + no seleccionable si solo está en la nube |

**Ronda 3** (validación del asesor, Fable) — diseño validado con estas correcciones aplicadas:
1. Caché de un navegador abierto: no se preselecciona y se pide "Cierra Chrome para liberar hasta X GB". El borrado POSIX con `FILE_SHARE_DELETE` daba falsos "liberados" y corrompía el índice de la caché.
2. `%TEMP%` solo borra lo de más de 7 días (`min_age`).
3. "Vaciar Papelera" se ejecuta antes de mover elementos a la Papelera.
4. Hard links: el que conserva el tamaño es el de ruta menor en orden lexicográfico (determinista), no el de id menor.
5. Menores:
   - Histogramas para todos los nodos (12 B).
   - Cadena de ancestros `INCOMPLETE` en la cola prioritaria y adopción por `FileId`.
   - `$Recycle.Bin`, `Default` y `Public` denegadas.
   - El titular suma solo lo Seguro.
   - El MVP borra con `remove_file` y deja POSIX para la v0.2.

**Ronda 13** (auditorías externas de Astra y Gemini sobre `v0.1.3`, verificadas por el coordinador; dos construcciones Sonnet en paralelo con alcances disjuntos —limpieza y resto—, auditorías Sonnet independientes con corrección de sus hallazgos, revisión con el asesor). Correcciones de la v0.1.4:
- **Envío a la Papelera (cuarta ronda de la misma familia de concurrencia):** vigilancia de cambios desde antes del recorrido hasta `PreDeleteItem`, huella idéntica (también encoger vuelve a la confirmación), ocupación leída dos veces y cargo con los reparse points de dentro (§3.7.4). Se corrige la afirmación de que el margen del 90 % absorbía los cambios de ocupación.
- **Aprobación:** un ancestro enlazado se rechaza (`ReparseAncestor`, también bajo un volumen montado en una carpeta); OneDrive se tolera como ancestro (la primera versión lo rechazaba y la auditoría lo detectó). Hijos abiertos relativos al directorio listado. `C:` sin barra deja de ser raíz de unidad (clave inválida, denegada).
- **Vaciar la Papelera:** un `$R` con enlaces dentro, o que no se pudo comprobar entero, se excluye antes de entregarlo ("no purgado", registro `linked`) sin cortar el lote; el veto del receptor queda como última defensa. Un `$I` ilegible se salta, consta (`bin-unlisted`) y no entra en lo prometido. Eventos y vetos se casan por nombre exacto antes que por `PathKey` (directorios sensibles a mayúsculas). Volúmenes montados en carpetas reconocidos en la protección, la Papelera y el tipo de unidad.
- **Escáner:** la cola prioritaria valida cada candidata componente a componente desde la raíz (sin seguir enlaces, solo `DirAction::Walk`, nombres 8.3 expandidos, `..` descartado); raíz `\\?\Volume{GUID}\` sin letra como volumen completo; `reparse_tag` solo con `REPARSE_POINT`; sin lote con los ids agotados; cachés de navegador verificadas para el escáner y candidatas para el `cleaner`. Efecto aceptado: una carpeta conocida bajo una carpeta de nube poblada deja de ser prioritaria.
- **Modelo y UI:** `Overlay::add_up` resta a los ancestros lo realmente restado; sumas saturadas; WTF-8 rechaza los 3 bytes sobrelargos; medidas obsoletas descartadas tras limpiar durante el escaneo. Sesión aislada (`--idle-memory`, `--bench-hover`, con o sin `--demo`) sin historial ni raíces de limpieza reales, y siempre simulación si no es la demo (M4 solo se cubre con pruebas unitarias: `SHGetKnownFolderPath` ignora las variables de entorno).
- **Pedido en las pruebas del usuario:** las unidades de la barra lateral se ven pulsables (fondo, cursor, tooltip; la actual no se anuncia como botón) y hay botón **Inicio** con `Alt+Inicio` en la barra superior, deshabilitado durante una limpieza. "Analizar una carpeta…" en la barra lateral se descartó: rompía la leyenda a 1280×800; se llega por Inicio.
- **Herramientas e instalador:** `privacy-check` (límite de tamaño primero, BOM acumulados, UTF-8 sin NUL, cadenas de 2+, autor y committer con su etiqueta; al regenerar una captura, el hash de la anterior va a `$legacyDemoImages`); `check-pe.py` exige `DependentLoadFlags` 0x800 y REPRO; GUID fijo del acceso directo; validaciones nuevas del MSI. `INSTALLFOLDER` **no** es propiedad segura: el MSI no tiene interfaz y es por usuario; marcarla segura solo valdría en una instalación elevada, donde dejaría al usuario dirigir dónde escribe el servidor. CODEOWNERS ampliado y `cargo deny --locked` con `all-features`. Para quien active SignPath: firmar los `.exe` **antes** de empaquetar el MSI y el zip.
- **Incidente y regla:** el antivirus del equipo volvió a retirar `cargo.exe` mientras otro auditor ejecutaba las pruebas que lanzan copias del programa con archivos con nombre de DLL al lado. Esas pruebas solo corren con `TESELA_PROCESS_TESTS=1`, que define el CI; en CI faltar la variable es un fallo, no un salto. En local se trabaja con la toolchain anterior hasta que el usuario restaure la actual; el CI confirma con la fijada.
- **Pendiente:** repetir `medicion_a_m3_*` (las cifras del registro son de la v0.1.3, antes de que `over_cap` tomase el cerrojo del escritor); spike de `FOF_WANTNUKEWARNING` con ventana propietaria (v0.2).
- **Pasada real única** (Papelera real, solo elementos propios, una vez y en serie, en el árbol fusionado): las 279 pruebas del `cleaner` en verde, incluidas las tres nuevas con la shell real (una carpeta de tres niveles queda `Recycled`, lo que confirma que la shell no toca el árbol antes de `PreDeleteItem`; un cambio justo antes de `PreDeleteItem` se veta; un `$R` propio con un junction dentro se excluye y el resto se purga). Papelera del usuario intacta.

**Ronda 12** (tercera auditoría externa de Astra sobre `v0.1.2`, verificada por el coordinador; construcción Sonnet, auditoría Sonnet con re-verificación, revisión con el asesor). Correcciones de la v0.1.3:
- **A2-B1:** dentro de una misma operación el presupuesto podía quedar obsoleto (una carpeta ya medida crecía mientras se medía otra). Ahora un directorio va siempre solo y los archivos del lote se vuelven a medir por su guarda en `PreDeleteItem` (§3.7.4).
- **A2-M1:** un fallo de la shell tras una pérdida parcial dejaba seguir el resto del lote; ahora un resultado de fallo corta el lote.
- **A2-M2:** la poda de un escritor podía borrar el registro abierto de otro; ahora el registro abierto no admite borrado compartido (§3.7.13).
- **A2-m1:** una comprobación imposible se presentaba como pérdida; ahora hay tres estados (intacto, cambiado, desconocido).
- La auditoría interna añadió pruebas negativas para el presupuesto de un directorio al entregarlo y para la comprobación de tamaño sin tolerancia, y textos que afirman la diferencia y no un borrado seguro.
- **Incidente:** una campaña de mutantes del auditor ejecutó el binario de pruebas del borrado en bucle y el antivirus del equipo lo marcó como malware (bloqueó 19 aplicaciones, retiró `cargo.exe` y reinició el equipo). Regla nueva: en la máquina del desarrollador no se ejecutan mutantes en bucle ni pruebas con la Papelera real repetidas; eso va al CI o a una VM desechable. Riesgo de producto asociado: un antivirus podría desconfiar de `tesela.exe` al limpiar (ROADMAP).

**Ronda 11** (auditorías externas de Astra —borrado— y Grok —resto— sobre `v0.1.1`, verificadas por el coordinador; dos construcciones Sonnet en paralelo con alcances disjuntos, auditorías Sonnet con re-auditoría de cada corrección, revisión con el asesor). Correcciones de la v0.1.2:
- **Borrado (Astra):** A-B1 los archivos se vuelven a medir por su guarda justo antes de cada operación y se recomprueba el presupuesto; A-B2 una medida de directorio solo vale si es completa; A-M1 los fallos de la shell y "sin aviso" se juzgan como un posible borrado, con huella del contenido; A-M4 vaciar la Papelera lleva guardas por `$R` y un receptor que veta sustitutos; A-M2 la retención no puede borrar el registro recién abierto; A-M3 cada resultado se vuelca y comprueba; A-m1 los callbacks COM se prueban directamente. Además: no se limpia elevado (E1), textos que no prometen cifras ni ausencia de riesgo (T1), temporales a 7 días (E2), nota de `hiberfil` honesta (E3), las funciones que borran son `pub(crate)` y la red de seguridad de las pruebas reales solo toca elementos con forma `tesela-test-<uuid>`.
- **Resto (Grok):** G-m1 la raíz y las rutas prioritarias del escáner no siguen un reparse point (raíz-enlace → `ScanError::RootIsLink`); G-m2 `privacy-check` detecta rutas UNC con `/`; G-m3 las reglas de ruta rigen en `spike/` (dos archivos históricos con rutas sintéticas permitidos por SHA-256 exacto); G-S1 valores partidos por NUL; G-S2 `install-wix.ps1` comprueba el SHA-512 **antes** de instalar; G-S3 documentado (no hay forma robusta de limitar `INSTALLFOLDER` sin acción personalizada).
- **Endurecimiento de DLL** (hallado al investigar G-S3): `dxcompiler.dll` se sondeaba por nombre y varias importaciones estáticas se buscaban primero en la carpeta del exe (§3.9).
- Incidente de proceso: un auditor creó junctions a la raíz de una unidad y lanzó `rm -rf` sobre su carpeta (no borró nada; comprobado). Regla nueva para todos los agentes: nunca borrado recursivo sobre carpetas con enlaces, quitar los enlaces como enlaces y no crear enlaces a raíces de unidad.
- Producto: estudio de diferenciación y dos revisiones externas → [ROADMAP.md](ROADMAP.md).

**Ronda 10** (auditoría externa de Astra sobre `v0.1.0`, verificada por el coordinador; construcción Sonnet, auditorías Sonnet independientes con re-auditoría de cada corrección, revisión con el asesor). Correcciones de la v0.1.1:
- **B1:** un reparse point por encima de una raíz de categoría (`User Data`, `%TEMP%`) volvía borrable contenido protegido, porque la raíz canónica se convertía en exención y levantaba cualquier denegación por encima. Ahora la cadena se recorre por componentes sin reparse points, la aprobación exige la misma identidad que vio la cadena y cada exención levanta solo su zona declarada (§3.7.3, §3.7.12). Una categoría descartada se muestra como "no disponible" con su motivo.
- **B2:** el reparto de la Papelera falla cerrado (sin capacidad u ocupación, `NoRoom`); se elimina el 5 % asumido; los directorios se miden justo antes de su operación.
- **M1, M2, S1:** `PreDeleteItem` veta sustitutos; `PostDeleteItem` corta el lote en el primer borrado definitivo o sin ruta; `FOF_NO_CONNECTED_ELEMENTS`. Se corrige la afirmación de que la guarda impedía renombrar el propio elemento.
- **M3:** vaciar la Papelera comprueba la identidad de `$R` y `$I`; el `$I` huérfano solo se borra con pruebas completas.
- **M4, m2:** sin registro no hay limpieza; intención antes de cada operación; la simulación no poda registros reales.
- **m1:** `NumberOfLinks` exacto. **m3:** `leak-scan` y `privacy-check` detectan valores locales no ASCII (UTF-8, Latin-1, UTF-16 sin BOM) sin perder la detección UTF-16. **S2:** `msi-validate` fija el árbol de carpetas, las filas de Upgrade, el orden `FindRelatedProducts` < `LaunchConditions` y compara distinguiendo mayúsculas.
- Las auditorías internas añadieron: la aprobación de una raíz compara la identidad con la de la cadena; el juicio de "eliminado" pasa a ser por objeto y por contenido (una primera versión por nombre ocultaba borrados reales con la guarda abierta, reproducido con la shell real); pruebas sin COM del receptor; retención de registros por clase; nombres con `:` rechazados.
- **M5** (protecciones del repositorio: rulesets, protección de `main`, environment de release, releases inmutables) es configuración del mantenedor y queda en su lista.
- Comportamiento visible nuevo: en un volumen sin tamaño máximo de Papelera configurado, lo que iría a la Papelera pide borrado definitivo expreso.

**Ronda 9** (incremento 4, instalador y release: construcción Sonnet, 2 auditorías Sonnet, revisión con el asesor):
- WiX 5.0.2 (no 6/7, para no aceptar el acuerdo OSMF en nombre del usuario), por usuario, sin acciones personalizadas; `INSTALLFOLDER` persistido en HKCU para que actualizar y desinstalar respeten una carpeta personalizada.
- Icono dibujado por `build.rs` (el repo no versiona imágenes fuera de `docs/img/`) e incrustado con `winresource`, con VERSIONINFO.
- Cadena de suministro: acciones por SHA, permisos mínimos por job, attestation en un job aislado, verificación antes del borrador, `--locked`, WiX con `nuget.config` limpio y SHA-512 comprobado, binarios reproducibles.
- Los tests con la Papelera real van detrás de `TESELA_REAL_RECYCLE_TESTS=1` (los runners de GitHub no tienen Papelera de escritorio); en local se lanzan con la variable.
- La seguridad real de la publicación hoy es el borrador que verifica una persona: la configuración del repo (rulesets, protección de `main`, environment) queda para el mantenedor.

**Ronda 8** (incremento 3, el borrado: construcción Sonnet, 3 auditorías Sonnet con sondas de ataque, revisión con el asesor):
- El diseño "verificar y actuar sobre el mismo handle" resistió todas las carreras probadas (sustitución ×300, junctions en hijo/ancestro/raíz, hard links, renombrados, cancelación, matar el proceso).
- Bloqueantes corregidos: verificación de la Papelera por identidad (no por nombre; fallaba con `.lnk`/`.url` y con extensiones ocultas), solo unidades locales (el recurso administrativo UNC de la propia máquina esquivaba la protección), `TEMP` y exenciones (podían borrar datos protegidos).
- Cambio de diseño: se retira la lápida (§3.7.4); Papelera por ruta original con sink de eventos y handle de guarda; reparto por operaciones con presupuesto y corte de seguridad (una operación con un elemento que no cabía eliminaba definitivamente todo el lote).
- Pendiente de prueba manual antes/después de la v0.1: `HideFileExt=1` (por construcción no afecta), volumen sin `MaxCapacity`, directiva `NoRecycleFiles`, symlinks de directorio, unidades de red mapeadas. Diferido a la v0.2: coste cuadrático con árboles de miles de niveles y protección de carpetas de sistema en otras unidades.

**Ronda 7** (auditoría externa de Astra/ChatGPT sobre 6781a03, triada con el asesor):
- **B1 (diseño):** el borrado se rediseña como "identidad aprobada + actuar sobre el mismo handle" (§3.7): apertura exclusiva, FileId+volumen, recursión relativa a handles (patrón CVE-2022-21658), Papelera vía lápida renombrada por handle y verificación por elemento, Vaciar con alcance enumerado, hard links detectados por `NumberOfLinks`, "liberado" = diferencia medida del espacio libre.
- **M2:** las cachés de navegador son solo directorios concretos de caché; el perfil nunca lo es (§2.6).
- **M1:** `--json` y `--screenshot` no sobrescriben sin `--overwrite`. **M3:** los atajos respetan el control enfocado. **M5/M6:** `privacy-check` revisa el mensaje completo, rechaza formatos opacos y el CI revisa todos los commits del push/PR. **m1–m11 y N1:** en el ciclo de endurecimiento.
- **M4:** se generalizan las menciones al equipo de medición en la documentación (versión del SO y tipo de disco). No se reescribe el historial: no identifican el equipo.
- **N2:** el bloque de espacio no explicado se ordena por tamaño; no hay fuentes de reserva de egui.
- **Incremento 4:** CRT estático (`+crt-static`) y requisitos de MSI/SignPath añadidos a §6.
- **Ciclo de endurecimiento** (13 commits, construcción Sonnet + auditoría Sonnet + revisión con el asesor): M1–M6, m1–m11 y N1 corregidos; exe ≈ 11,0 MiB con el CRT estático. Por decisión del usuario NO se añaden patrones genéricos de versión de Windows ni de capacidad de disco: lo que importa es la información personal (nombres de carpetas y archivos, usuario, rutas del perfil, capturas reales).
- **Distribución:** decisión del usuario: **solo MSI** (no MSIX ni Microsoft Store). Firma con SignPath Foundation (gratis para open source) cuando el usuario la solicite; la v0.1 sale sin firmar con el aviso de SmartScreen explicado.

**Ronda 6** (incremento 2 del MVP: construcción Sonnet, 2 auditorías Sonnet, verificación propia y asesor):
- Memoria: el objetivo pasa a ~85 B por entrada en régimen (≈ 155 MiB con 1 M); **decisión del usuario**, no se recorta. En reposo con el mapa: ~68 MiB.
- Fuentes: sin `default_fonts` de egui (no se redistribuyen fuentes OFL/UFL); reservas DEL SISTEMA por escritura (CJK, tailandés, índicas, árabe, hebreo, símbolos), con kana y han siempre juntos. Las pruebas de fuentes se saltan lo que el equipo no tiene (CI en Windows Server).
- Avisos de terceros: `THIRD-PARTY-NOTICES.md` generado con `cargo about` (no se edita a mano), incrustado en el `.exe` y visible en Ajustes › Acerca de.
- "Otros": paginado cuando ningún hijo cabe, de modo que abrirlo siempre avanza. Esquinas de 4 px solo en celdas ≥ 16 px. Relayout de 20k celdas ≈ 1 ms (hito de la v0.2 ya cumplido).
- Capturas: `--screenshot` **exige `--demo`** (en código); `docs/img/` solo admite PNG registrados con su SHA-256 en `docs/img/README.md`, con fragmentos y longitudes acotados.
- Hooks sin `-ExecutionPolicy Bypass` (heurística del antivirus).
- egui 0.36.2 se mantiene (0.37 existe; subir es tarea de la v0.2). Mientras no haya riesgo (inc. 3), la cabecera del escaneo dice "X GB en sitios conocidos". Tabla de rutas conocidas con `KnownKind`.
- Pendiente para la v0.2: profundidad de "Otros" anidados en colas muy pesadas (R4) y validar el zlib de `IDAT` y los data URI en `privacy-check` (R6). Las herramientas de medición (`--bench-hover`, `--idle-memory`) pueden fallar si la ventana queda tapada: es del entorno, no de la app.

**Ronda 5** (incremento 1 del MVP: construcción Sonnet, 2 auditorías Sonnet, revisión con el asesor):
- `Hist` de 14 B con `dominant_type` / `dominant_age`; regla del 80 % precisada; `mtime` 0 fuera de la antigüedad.
- Memoria: régimen ~78 B por entrada; pico ≈ 2× el régimen.
- `Record` de 32 B con el `FileId` aparte; dedup tras el BFS y también con `logical = 0`.
- Respaldo `FindFirstFileExW`: sin dedup ni cola prioritaria, escaneo "aproximado".
- `INCOMPLETE_BELOW`; sin bloque de espacio no explicado si hay directorios incompletos.
- Carpetas de la nube: pobladas se recorren, no pobladas se cuentan sin entrar.
- Hilos `min(16, 2 × núcleos)`; `cargo test` en vez de nextest; binarios publicados solo con `build-release.ps1`.
- Privacidad: `privacy-check.ps1` con hooks `pre-commit` y `commit-msg`, lista negra local en `<git-common-dir>/privacy-denylist`, pruebas propias y job de CI.

**Ronda 4** (resultados de la fase 0: construcción Sonnet, auditoría Sonnet, revisión final con el asesor):
- **UI:** wgpu solo DX12, treemap como `Mesh` cacheado (objetivo p99 < 8 ms), fondo sólido (sin Mica).
- **Métricas:** memoria = private working set (≤ 75 MiB en reposo con el mapa); exe ≲ 16 MB.
- **Borrado:** POSIX propio descartado; la nota de `FILE_SHARE_DELETE` se amplía a `remove_file`/`DeleteFileW`; junctions medidos.
- **Carpetas de desarrollo:** destino diferido a la v0.2, con "renombrar primero".
- **Hilos:** se sigue ajustando midiendo (la mejora se satura pronto).
- **Deshacer:** verificar archivo a archivo.
- **Entregable:** MSI por usuario desde la v0.1 (decisión del usuario).

**Ronda 2** (cierre de las decisiones cruzadas): carpetas de desarrollo a la Papelera por defecto (la medición de la fase 0 puede cambiarlo), histogramas, pagefile/hiberfil como nodos "solo informar", `wgpu` por defecto y cesta persistida al elevar. Ni diseño ni arquitectura tienen objeciones bloqueantes.
