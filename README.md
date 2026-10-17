# Tesela

**See what takes up your disk and decide what to free, knowing what gets deleted and checking what was actually freed.**
A visual disk space analyzer for Windows, written in Rust: fast, light and clear. The application is available in Spanish and English.

[Installation](#installation) · [Code signing policy](CODE_SIGNING.md) · [Security](SECURITY.md) · [Changelog](CHANGELOG.md)

> 🚧 **Status: MVP v0.1 published (v0.1.4).** Done: the model, the scanner, the interface (map, list, details, scan with real progress, Spanish/English, light/dark) and **the cleaner**: Recommendations, basket, confirmation and summary, with safe handle-based deletion and sending to the Recycle Bin. **This version really deletes** (only what you approve in the confirmation): try it first with `--dry-run` or `--demo`. Per-user MSI installer in [Releases](https://github.com/wandoth1/tesela/releases) (see [Installation](#installation)); use **v0.1.4** or later: earlier versions had bugs in the cleaner. Phase 0 findings are in [docs/phase-0-findings.md](docs/phase-0-findings.md); the full plan is in [docs/PLAN.md](docs/PLAN.md).

## What Tesela is

- **Disk map (treemap) + list.** Each folder is a block proportional to the space it takes, colored by file type, age or risk.
- **An answer in seconds.** While it scans it already tells you "up to X GB in low-risk candidates" (an estimate, checked when cleaning), because it starts with the usual junk locations (temporary files, caches, Recycle Bin).
- **A cleanup assistant with visible risk.** Each suggestion is marked 🟢 Safe, 🟠 Review or 🔺 Info only, and explains in one sentence why.
- **Nothing is deleted blindly.** Everything goes through a review basket. What can be regenerated (temporary files, caches) is deleted; your own files go to the Recycle Bin. System folders are protected.
- **Honest with the numbers.** What gets freed is measured (the real change in free space) and counted separately from what was deleted and what was sent to the Recycle Bin, and it shows which part of the disk it could not measure.
- **Light.** A single binary, no Electron or WebView, no telemetry or accounts. It works without administrator rights and has an optional fast mode that reads the NTFS file table (MFT) directly.

## Installation

Windows 10 22H2 or Windows 11, x64. **No administrator needed.** Download the files of the latest release from
[github.com/wandoth1/tesela/releases](https://github.com/wandoth1/tesela/releases) (while none is published, the
installer can only be built from the source: `pwsh -File scripts/build-msi.ps1`).

**Code signing policy:** v0.1 is not signed yet; the [Code signing policy](CODE_SIGNING.md) describes what will be signed, who approves each release and how it is built.

### MSI installer (recommended)

1. Download `tesela-<version>-x64.msi`.
2. Double-click it. It installs **for your user only** in `%LOCALAPPDATA%\Programs\Tesela`, without asking for administrator
   rights, with a **Tesela** shortcut in the Start menu and an entry in *Settings > Apps > Installed apps*. (Silently:
   `msiexec /i tesela-<version>-x64.msi /qn`.)
   **Install folder:** with `INSTALLFOLDER=` the MSI installs wherever you tell it, **without checking who can write there**; choose a
   folder that only you (and administrators) can write to. If other users can write to it, they could leave a DLL next to
   `tesela.exe` that would be loaded when it starts (suspected risk, not demonstrated; the program already looks for its DLLs only in `System32`, but it is better not to depend on that; details in [installer/README.md](installer/README.md)).
3. **Update:** install the new version's MSI over it; it replaces the previous one without leaving duplicates. Installing an
   **older** version than the installed one is blocked with a message (uninstall the new one first if you really want to go back).
4. **Repair:** *Installed apps* does not offer "Modify" (there is nothing to choose); to repair, run
   `msiexec /fa tesela-<version>-x64.msi` with the MSI of the installed version.

### Portable zip

`tesela-<version>-x64-portable.zip` contains `tesela.exe`, `tesela-scan.exe`, the licenses and the third-party notices. Unzip it
into a folder **in your profile** (not in the root of a drive nor in a folder other users can write to: Windows looks for certain DLLs first next to the `.exe`) and run `tesela.exe`; it installs nothing and does not touch the registry. To remove it, delete the folder.

> The downloaded zip carries the *Mark of the Web*, and the files you unzip inherit it, so SmartScreen may also warn when you open the unzipped `tesela.exe` (see below). If you have verified the download, you can remove the mark before unzipping with *Properties > Unblock* on the zip, or with `Unblock-File .\tesela-<version>-x64-portable.zip`.

`tesela --version` and `tesela-scan --version` print the version. `tesela-scan` (a console tool) writes it to **standard** output; `tesela.exe` (a graphical application, with no console of its own) writes it to the **error** output of whatever launched it, so in PowerShell you can see it with `Start-Process .\tesela.exe -ArgumentList --version -Wait -NoNewWindow` or from `cmd`.

### Verify the download

Each release includes `SHA256SUMS.txt` and a provenance *attestation* (GitHub certifies that the files come from this repository's
public `release.yml` workflow, untampered).

```powershell
# 1. The SHA-256 sum has to be the one in SHA256SUMS.txt:
Get-FileHash .\tesela-0.1.0-x64.msi -Algorithm SHA256
# 2. The provenance (needs GitHub CLI: https://cli.github.com):
gh attestation verify .\tesela-0.1.0-x64.msi --repo wandoth1/tesela `
    --signer-workflow wandoth1/tesela/.github/workflows/release.yml `
    --source-ref refs/tags/v0.1.0
```

`--source-ref refs/tags/vX.Y.Z` checks that the file was built from THAT tag (change `v0.1.0` to the version you downloaded); `--signer-workflow` checks that it was signed by this repository's `release.yml` workflow.

### Windows SmartScreen warning

v0.1 is **not signed yet** (signing with [SignPath Foundation](CODE_SIGNING.md) will be requested later), so
Windows SmartScreen may show *"Windows protected your PC"* when you open the MSI or the `.exe`: it does not know the publisher or the file,
it has **not** detected anything. If you have verified the download (above):

1. Click **More info**.
2. Click **Run anyway**.

(If the browser blocks the download, use *Keep* in its downloads list.) With the signed version the warning will go away
as it gains reputation. If your antivirus quarantines the installer, do not add exceptions blindly: open an issue with the
name of the detection.

### Uninstall and what stays behind

*Installed apps > Tesela > Uninstall* (or `msiexec /x tesela-<version>-x64.msi`). This removes the program folder,
the Start menu shortcut, the Installed apps entry and the values the installer wrote to
`HKCU\Software\Tesela`.

**Your data is not deleted** (the installer does not know about it; it is yours):

- `%APPDATA%\Tesela\config.toml`: your settings (theme and language).
- `%LOCALAPPDATA%\Tesela`: `history.jsonl` (cleanup summaries) and the logs `limpieza-<date>-<time>-<n>.jsonl` (the path and identity
  of every item cleaned; `limpieza` is Spanish for "cleanup").

To delete them by hand after uninstalling (or before, with *Settings > Delete the cleanup history and logs*, which removes the history and the logs):

```powershell
Remove-Item -Recurse -Force "$env:APPDATA\Tesela", "$env:LOCALAPPDATA\Tesela"
```

## Usage

Windows 10 22H2 and Windows 11. The application **only deletes or moves what you approved in the confirmation** (never while scanning, nor in `--demo`). It keeps its settings in `%APPDATA%\Tesela\config.toml` and, in `%LOCALAPPDATA%\Tesela`, a log of every cleanup (`limpieza-<date>-<time>-<n>.jsonl`: path, identity and result of each item; only on your computer, the last 30 are kept and it is flushed to the system after every result) and `history.jsonl` with summaries (date, drive and figures, no paths). "Delete the cleanup history and logs", in Settings, deletes both.

```powershell
tesela                      # home screen: choose a drive or "Analyze a folder…"
tesela --scan C:\           # scan that drive (or folder) as soon as it starts
tesela --scan C:\ --dry-run # simulation mode: does everything except delete or move (measures and logs what would have happened)
tesela --demo               # demo mode: SYNTHETIC tree and fictional drives; it does not read your disk or delete anything
tesela --help
```

`--demo` is the only mode used for screenshots and tests: everything it shows (folders, sizes, drives) is made up and always the same. It accepts `--screen home|scan|map|list|recs|confirm|summary`, `--open Users/usuario`, `--select Descargas`, `--stopped` (a stopped scan, with incomplete folders), `--theme light|dark`, `--lang es|en` and `--size 1280x800`. (The names in the demo tree, such as `usuario` and `Descargas`, are in Spanish, and the screenshots below use the Spanish interface.)

![Map of a user folder in demo mode, light theme](docs/img/mapa-claro.png)
![Map of a drive in demo mode, dark theme](docs/img/mapa-oscuro.png)
![Recommendations in demo mode: the safe items already checked, what is worth reviewing and what is only explained](docs/img/recomendaciones.png)
![Confirmation before cleaning, with what gets deleted and what goes to the Recycle Bin](docs/img/confirmacion.png)
![Summary of a cleanup: the measured change in free space, what was deleted and what was moved to the Recycle Bin](docs/img/resumen.png)

- **Map:** one block per folder or file, proportional to the space it takes **on disk**, colored by type (a color-blind-friendly palette). The next level is hinted inside each block. Blocks that are too small are grouped into **"Others (N items, X)"**, which opens with a double-click. Folders the scan did not manage to walk in full carry diagonal stripes and a "≥".
- **Grays with meaning:** the "Windows and other system data" block (what takes up the disk but your user cannot see), `pagefile.sys` and `hiberfil.sys` ("Info only") and "Mixed" (folders with no dominant type).
- **List:** the same folders with exact figures; it is the accessible view (screen reader) and shares the selection with the map.
- **Keyboard:** double-click or `Enter` enters a folder; `Backspace` goes up; the arrow keys move the selection; `Ctrl+F` searches; `F5` rescans; `Alt+Home` goes back to the home screen. Right-click: open in Explorer, copy the path and Properties.
- **Settings (⚙):** theme (same as system, light or dark), language (same as system, Español or English) and delete the cleanup history.

### Cleaning

- **Recommendations:** temporary files of the user and of Windows (only what has not been modified in the last 7 days), Edge, Chrome and Firefox caches (with the browser closed), and, as **Review**, Downloads and the Recycle Bin. `pagefile.sys`, `hiberfil.sys` and `swapfile.sys` are only explained. "Up to X in low-risk candidates" adds up **only** what is Safe, and it is an estimate, not a guaranteed minimum: the real figure is the one the cleanup measures.
- **Everything goes through the basket and a confirmation** that groups by destination: what can be regenerated is **deleted** (it is recreated on its own) and your own files go to the **Recycle Bin**. What does not fit in the Recycle Bin (or goes to a disk with no maximum Recycle Bin size configured: it is set in the Recycle Bin properties) is shown separately, in amber, and is only deleted if you explicitly check it. Your five biggest own files are always named.
- **Protected:** Windows, Program Files, the whole of `AppData`, the key folders of the profile (only their contents are cleaned), Tesela itself (even if it is inside the temporary folder) and whatever cannot be checked. Only **local drives** are cleaned: a network path, even one that points to a disk of your own computer (through `localhost` and its administrative share), is rejected. It is shown with a padlock and a dotted border and cannot be added.
- **Safe deletion:** the identity of each item (volume, file ID and links) is checked right before touching it, it is opened exclusively and deleted by handle, never entering reparse points (junctions, symbolic links); a hard link does not count as freed. What is in use is **skipped**, not forced. What goes to the Recycle Bin is checked the same way and handed to Windows **by its original path** (without renaming it first), with a guard handle open that prevents its folders from being swapped for a link in the meantime, and right before Windows acts it is checked once more that it is the approved object (if not, it is not sent). Windows reports what happened with each item and Tesela compares the **identity** of what reached the Recycle Bin with the approved one (and that the approved name no longer exists). If someone replaced it right in the middle, what arrives is usually a different object: you are told, and it can be restored from the Recycle Bin; but if that object does not fit, Windows deletes it, and that minimal window cannot be fully closed. Items are split into operations according to the space left in the Recycle Bin (folders are measured right before), and what does not fit or cannot be guaranteed is not sent; if Windows ever deleted something permanently, sending stops at that moment and nothing else is touched. Without a local cleanup log nothing is deleted. While it runs, closing the window is blocked ("Wait for it to finish or cancel"). If Tesela runs as administrator, real cleaning and emptying the Recycle Bin do not work (for safety); scanning and simulation do. With User Account Control (UAC) turned off, or with the built-in Administrator account, every process counts as elevated and Tesela will refuse to clean: use a standard user account (or with UAC on) and open it with a normal double-click, not "Run as administrator".
- **"Measured change in free space"** is what actually changed in the drive's free space during the cleanup (other programs writing at the same time also move it); what was deleted and what was sent to the Recycle Bin are counted separately, because what is sent does not free space until the Recycle Bin is emptied.
- The tree is updated **without rescanning** (the histograms are marked as approximate until you rescan).

## Roadmap

| Version | Content |
|---|---|
| Phase 0 ✅ | Measurement tests: graphical interface, folder-reading API, Recycle Bin (done: [findings](docs/phase-0-findings.md)) |
| v0.1 | Scan, map + list, basket, cleaning of temporary files, caches and Recycle Bin. Deliverable: **per-user MSI installer** from v0.1 |
| v0.2 | Full assistant (project folders, old downloads, large files), undo |
| v0.3 | Fast mode reading the MFT (administrator) |
| v1.0 | Duplicates, incremental rescan, signed installer, winget, ARM64 |

## Technology

Rust · [egui/eframe](https://github.com/emilk/egui) · [windows-rs](https://github.com/microsoft/windows-rs) · Windows 10 22H2 and Windows 11.

## Development

Cargo workspace at the root (`crates/`); `spike/` is the phase 0 measurement code, with its own workspace.
State of the MVP v0.1: done are the model (`tesela-model`), the read-only scanner (`tesela-scanner`) with its CLI
`tesela-scan`, the treemap layout (`tesela-layout`), the cleaner (`tesela-cleaner`: rules, protections, deletion by
handle and Recycle Bin), the interface (`tesela-ui`) and the executable (`tesela-app`, binary `tesela`).

Requirements: Rust (the toolchain in `rust-toolchain.toml`, MSVC), the **Windows SDK** (`rc.exe`: `winresource` embeds the icon and the version data in the `.exe` files), Python 3 (`build-release.ps1` uses it to read the PE imports), **PowerShell 7** (`pwsh`) and, for the installer, **WiX 5.0.2** as a `dotnet tool` (`pwsh -File scripts/install-wix.ps1 -Version 5.0.2`; see [installer/README.md](installer/README.md)). For the **full** test suite, set `TESELA_REAL_RECYCLE_TESTS=1` (tests with the real Recycle Bin) and `TESELA_PROCESS_TESTS=1` (tests that launch child processes); see below.

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # the tests only touch test folders in %TEMP%\tesela-test-<uuid> (and, for the Recycle Bin, only their own items)
$env:TESELA_REAL_RECYCLE_TESTS = '1'; $env:TESELA_PROCESS_TESTS = '1'; cargo test --workspace   # the FULL suite: adds the tests with the REAL Recycle Bin and with child processes (see below)
cargo deny check
cargo run --release -p tesela-scan -- <path> --top 20   # read-only; options in the header of crates/tesela-scan/src/main.rs
cargo run -p tesela-app -- --demo                        # the interface with synthetic data
cargo test -p tesela-layout --release -- --ignored relayout   # layout benchmark (20k rectangles)
pwsh -File scripts/build-release.ps1                     # release WITHOUT machine paths in the binaries (and it checks that)
pwsh -File scripts/build-msi.ps1                         # per-user MSI + portable zip + SHA256SUMS.txt (needs WiX 5.0.2: installer/README.md)
python -I -m unittest discover -s scripts/tests -p "test_*.py"   # tests of scripts/check-pe.py
```

README screenshots (only from `--demo` mode; see [docs/img/README.md](docs/img/README.md)):
`cargo run -p tesela-app --features capture -- --demo --size 1280x800 --screenshot output.png` (it does not overwrite a file that already exists: `--overwrite` replaces it). The `capture` feature is not
part of the executable that is published. `--bench-hover <seconds>` moves a synthetic pointer over the map and prints the CPU
cost per frame, and `--idle-memory <seconds>` leaves the window idle and prints the process's private working set (without PowerShell, CIM or WMI); the results of any measurement stay out of the repository.

**Tests with the real Recycle Bin.** The `cleaner` tests that send items to the user's Recycle Bin (and check that
Windows delivers them with their identity) only run with `TESELA_REAL_RECYCLE_TESTS=1`; without it they are skipped with a message. GitHub's CI
(a Windows Server with no interactive session) has no reliable user Recycle Bin and the tests failed there because of the environment, not because of
the code. Run the full suite with the variable on your machine before touching `crates/cleaner`. They only use their own items
(filtered by the test's uuid) and do not empty the Recycle Bin.

**Tests with child processes.** The tests that launch copies of `tesela.exe` / `tesela-scan.exe` (with empty files named
like DLLs next to them, to check that they are not loaded), open and close the `--demo` window or run the `tesela-scan` CLI only
run with `TESELA_PROCESS_TESTS=1`; without it they are skipped with a message (the assertions have not been weakened). A desktop antivirus
may block that pattern (executables copied next to fake DLLs, windows that open and close); that is why they are not enabled by default locally. CI always sets it
(`.github/workflows/ci.yml`, job `build`), so there they run on every push to `main` and on every pull request. The
unit tests of `dll_hardening` that do not launch processes and the PE reading (`DependentLoadFlags`, manifest) do not depend on the variable.

The release links the C runtime statically (`.cargo/config.toml`, repeated in `build-release.ps1` because
`CARGO_ENCODED_RUSTFLAGS` takes priority over the config): the `.exe` does not import `VCRUNTIME*.dll`, `MSVCP*.dll` or
`api-ms-win-crt-*.dll`. On Windows 10/11 the UCRT belongs to the system; it is linked statically so as not to depend on the redistributable
VC++ runtime, which a per-user installer without administrator cannot install. `build-release.ps1` takes the executables from Cargo's JSON output,
requires `tesela.exe` and `tesela-scan.exe` (it fails if one is missing, another appears or an old one is left in the folder) and
reads the PE import table with `scripts/check-pe.py` (needs Python 3).

### Privacy

The repository is public and **cannot contain data from any machine**: hardware, system, user, paths, sizes of
real disks or scan results (examples, screenshots and fixtures are always synthetic).

- **Install the hooks after cloning:** `pwsh -File scripts/install-hooks.ps1`. `pre-commit` checks what is staged
  (index contents and file names) and `commit-msg` checks the message. They require **PowerShell 7 (`pwsh`)**; without it, or
  without `scripts/privacy-check.ps1` in the worktree, the commit is blocked with a clear message.
- `scripts/privacy-check.ps1 -All` checks the whole tree (CI runs it with `-GenericOnly`). It looks for SIDs, real profile
  paths, drives D:–Z:, network UNC paths (`\\<server>\<share>`: in the documentation use a placeholder between `<>`), hardware,
  antivirus and Windows builds; locally it adds the user, the machine and the profile read
  from the environment at run time. It decodes UTF-8 (and, if the bytes are not valid UTF-8, also Latin-1/ANSI, each decoding separately with its line numbers), UTF-16 and UTF-32 with BOM, and for binaries it checks their strings (ASCII, and ASCII with letters of any alphabet, in Latin-1 and in UTF-16 of both alignments); opaque or compressed
  formats are rejected **by their signature** (ZIP and derivatives, also by their end record in the last 64 KiB, gzip, 7z, rar, xz,
  bz2, zstd, cab, msi, pdf, sqlite: their contents cannot be checked) and images are rejected, with ONE narrow exception: `docs/img/<name>.png` if it is a PNG without metadata (only IHDR, PLTE, tRNS, IDAT, IEND, sRGB, gAMA, cHRM and pHYs chunks), under 2 MiB, and `docs/img/README.md` has a line with its name and the `--demo` command that generates it.
  `docs/spike-results.md` cannot exist and `spike/` passes the rules not tied to paths.
- **Limits that no signature detects:** raw zlib, lzma-alone (`.lzma`), encrypted or obfuscated data and text in base64 or any other
  transformation. They are held back by the repository not accepting unknown binaries and by whoever audits the change seeing them. UTF-8 (with or without BOM), UTF-16 and UTF-32 (with BOM) are
  decoded and, on any NUL in the WHOLE file, its strings (ASCII and ASCII with letters of any alphabet) are also checked in Latin-1 and in UTF-16. Local values with accents are searched in
  all those encodings with word boundaries that count accented Latin letters; a value attached to a
  Cyrillic, Greek or CJK letter counts as a match (a false positive, on the safe side). *Known limits, unchanged:* NFC/NFD is not
  normalized, bytes 0x80-0x9F of CP1252 are not interpreted, a value of 3 to 5 characters is only seen in the UTF-16 of a
  binary if it is inside a string of 6 or more, and UTF-16 big endian without BOM is only seen for Latin letters (Latin-1).
- **Messages and ranges:** the commit message is checked in full (also lines with `#` and what follows the scissors line
  of `git commit --verbose`, even though git would trim it: the hook sees the file before that). `privacy-check.ps1 -From <sha> -To <sha>`
  checks the message of every commit and ALL the blobs the range introduces, including those a later commit deletes; CI
  runs it on every push to ANY branch or tag (`before..sha`; with `before` all zeros or absent, which is what happens on a new
  branch or a tag, the whole history) and on every pull request (`base..head`). In a range, a PNG in `docs/img/` is accepted
  if its SHA-256 appears in the `docs/img/README.md` of its own commit or of the final commit (`-To`). Type changes (a file
  that becomes a symbolic link, whose target is its content) are also checked.
- **Local denylist (optional, not versioned):** create `<git-common-dir>/privacy-denylist` (in a normal clone,
  `.git/privacy-denylist`; in a worktree, the `.git` of the main repository; `git rev-parse --git-common-dir` tells you) with
  **one term per line**. Literal, case-insensitive match; empty lines and those starting with
  `#` are ignored. It is for proper names (folders, programs, machines) that no generic pattern can know.
- Tests of the check itself, with `pwsh` and with Windows PowerShell 5.1:
  `pwsh -File scripts/tests/privacy-check.tests.ps1` and `pwsh -File scripts/tests/privacy-check.tests.ps1 -Shell powershell`.
- `scripts/build-release.ps1` compiles with a `--remap-path-prefix` computed at run time (cargo's `trim-paths` is still
  unstable and the prefix depends on the machine, so it does not fit in `.cargo/config.toml`) and fails if the `.exe` contains the
  user, the profile, the repository or the toolchain.

### Scanner design notes

- Cloud folders (OneDrive and similar): the **already populated** ones (a reparse point with a cloud tag) are walked; the **unpopulated**
  ones (attributes `RECALL_ON_OPEN` or `RECALL_ON_DATA_ACCESS`) are **not walked**, so as not to trigger network downloads: they are
  counted with their `AllocationSize` and marked `CLOUD | INCOMPLETE` (the CLI shows them as `NUBE`, Spanish for "cloud"). The remaining directory reparse
  points (junctions, symlinks, mounts) are not followed. There is no way to create real placeholders in CI: only the
  classification and the builder are tested, with synthetic attributes.
- The `mtime` of a directory in its parent's listing is refreshed lazily on NTFS: right after writing it can be
  stale. Its own size is 0 in the parent's listing (the real space the directory takes goes to the system data block, that is,
  to the "unexplained space").

- **Volume space and disk quotas:** `NtQueryVolumeInformationFile` gives three magnitudes of different scopes, in
  this query order: `FileFsFullSizeInformationEx` (class 14, Windows 10 1809+: `ActualTotalAllocationUnits` and
  `ActualAvailableAllocationUnits` belong to the VOLUME, `CallerAvailableAllocationUnits` belongs to the user); if it is not supported,
  `FileFsFullSizeInformation` (class 7), whose `TotalAllocationUnits` with a quota is what is available TO THE USER and so
  is only valid as volume capacity when `CallerAvailable == ActualAvailable`; and finally `GetDiskFreeSpaceExW`, with the same
  condition. The "unexplained space" (`total - free - scanned`) uses ONLY volume-scope figures: with a quota and without
  class 14 there is no remainder (subtracting different scopes would give an invented number), and the drive card shows the user's
  figures with the notice "Figures for your account (disk quota)". With volume scope and a quota that limits, the card
  adds a separate line "Available to your account (quota)". The usage ring and "X free of Y" always use two figures of the
  same scope.

### Third-party licenses

- **Rust dependencies:** [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) lists every component compiled into the `.exe` with the text of its license. It is generated with [`cargo about`](https://github.com/EmbarkStudios/cargo-about) and the configuration in `about.toml` and `about.hbs`; it is not edited by hand. To regenerate it after changing dependencies: `cargo install cargo-about --locked --features cli` (once) and `cargo about generate --locked -o THIRD-PARTY-NOTICES.md about.hbs`. The file is **embedded in the executable** and can be read from the application itself: Settings, "About Tesela", "Third-party notices".
- **Icons:** a subset of 30 of [Fluent UI System Icons](https://github.com/microsoft/fluentui-system-icons) (© Microsoft, MIT), as they come from their official repository, with their `LICENSE` in `crates/ui/assets/icons/`.
- **Fonts:** the interface uses Segoe UI (Variable) from Windows itself, which is loaded at run time and **not redistributed**. The `.exe` **carries no font** (egui is compiled without `default_fonts`, so there are no OFL or Ubuntu fonts either). Names in Chinese, Japanese, Korean, Thai, Indic scripts, Arabic, Hebrew or with symbols load, only if they appear, the matching system fallback font (Microsoft YaHei or MS Gothic, Malgun Gothic, Tahoma, Nirmala UI, Segoe UI and Segoe UI Symbol). If Windows had no usable font, the application says so and closes. Color emoji cannot be drawn (egui does not support them).

## License

Dual license, at your choice:

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

Unless you state otherwise, any contribution you submit for inclusion in the project is under that same dual license, with no additional terms.
