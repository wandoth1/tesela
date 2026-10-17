# Changelog

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow [SemVer](https://semver.org/)
(`major.minor.patch`, no suffixes: an MSI does not allow `-rc1`). `release.yml` copies the section of the tag's version to the
notes of the GitHub Release, so each version needs its section here.

## [0.1.6] - 2026-10-18

Security hotfix for the v0.1.5 beta. Cleaning changes files: review the confirmation, especially any permanent-deletion
warning. The binaries remain unsigned, so Windows SmartScreen may warn.

### Fixed

- Deselecting the option to permanently delete items that do not fit in the Recycle Bin now revokes the separate local-mail
  acknowledgment when its checkbox disappears. The final execution permission is also checked against the visible,
  actionable plan, so an acknowledgment from an earlier selection cannot authorize a `.pst` found later inside a folder.
- A diagnostic `.pst` that must stay in place because it cannot fit in the Recycle Bin no longer triggers a misleading
  permanent-deletion acknowledgment. Diagnostics remain Recycle Bin only.

## [0.1.5] - 2026-10-17

Beta release with a setup interface, multiple selection and clearer cleanup decisions. **Cleaning still changes files**: review
the confirmation, especially any permanent-deletion warning. The binaries remain unsigned, so Windows SmartScreen may warn.

### Added

- The MSI now offers installation only for the current user or for all users, a folder chooser, and an optional desktop
  shortcut, with Spanish and English setup text. A personal install needs no administrator permission; an all-users install
  requests it when installation begins. This does not elevate Tesela when it cleans.
- Select several visible files or folders before adding them to the basket: Ctrl+click, Shift+click or arrows, Space, Ctrl+A,
  and Invert selection. A selection bar shows the count, size and why an item cannot be added. The basket and its view stay
  responsive with large selections.
- Two optional **Review** categories: per-user Windows error reports and application crash dumps. Only items older than
  seven days are offered; they are sent to the Recycle Bin, never deleted permanently, and are never preselected.
- An optional update check in Settings, off by default. At most once per day it asks GitHub for the latest published release
  and offers to open its page; it never downloads or installs anything. It sends the fixed `User-Agent: Tesela`, while GitHub
  sees the request's IP address. See the README's Privacy and network section.

### Changed

- Cleanup preparation and completion no longer perform log creation, history writes or process enumeration on the UI thread.
  Approval shows progress, and a long file deletion explains why cancellation waits for that file. The confirmation no longer
  offers an active cleanup button when every item awaits permanent-deletion consent.
- The confirmation distinguishes Outlook cache files (`.ost`/`.nst`) from local mail data (`.pst`), warns if Outlook is open,
  and requires a second explicit acknowledgment before permanently deleting a `.pst`.
- Diagnostics that are too recent or cannot fit in the Recycle Bin stay in place with a recorded reason; a basket containing
  only these skipped items does not offer a no-op cleanup.

### Fixed

- The MSI's dual-scope metadata allows a standard user's personal installation without elevation and rejects an all-users
  installation when credentials cannot be requested. It also handles a personal installation in the presence of an all-users
  copy without silently changing scope.
- Confirmation now shows the size of items awaiting permanent-deletion consent instead of suggesting that the basket holds
  zero bytes.
- A `.pst` added inside an approved folder after confirmation is now skipped unless local-mail permanent deletion was
  explicitly acknowledged. The check applies to each file when the folder is traversed.
- A diagnostic folder temporarily unavailable during approval can no longer lose its permanent-deletion veto; the veto uses
  fixed known-folder paths and is checked again during execution.

## [0.1.4] - 2026-10-17

Fourth round of fixes, after two external audits of 0.1.3, and two usability improvements requested during testing.
**Update if you use the cleaner.** It is still **unsigned**: Windows SmartScreen may warn.

### Added

- **Home** button (and `Alt+Home`) in the top bar of the browsing view: it goes back to the drive-selection screen. It is
  disabled while a cleanup is running.
- The drives in the sidebar look clickable: they are highlighted on hover, change the cursor and explain that they
  scan that drive.

### Security

- A folder being sent to the Recycle Bin is **watched** from before it is measured until the last moment: if anything
  inside changes (including if it shrinks), it is not sent and confirmation is requested again. It is only sent with the same
  contents you approved.
- The space available in the Recycle Bin is read before and after measuring, and the worse reading wins.
- Links and cloud files inside a folder count against the Recycle Bin space.
- On approval, anything hanging from a link (junction, symlink or a volume mounted in a folder) is rejected;
  folders synced with the cloud (OneDrive) are still accepted, with their warning.
- Emptying the Recycle Bin skips, with the reason, deleted folders that contain links, and empties the rest. An unreadable
  Recycle Bin item is skipped and recorded in the log.
- Disks mounted without a drive letter and recognized volume paths are covered by the system protection and by the scan.
- The priority scan no longer follows links anywhere in the path nor enters cloud folders.
- Demo mode and the measurement tools no longer read your history nor prepare a real cleanup.
- Extra hardening against planted DLLs (the current directory is removed from the search).

### Fixed

- Correct sizes when deleting inside a folder that had shrunk; map measurements that were not being updated.
- The `tesela-scan` tool accepts paths with any name and no longer blames the system for space it could not read.
- Installer: the shortcut has a fixed identifier and the MSI validator checks more things.
- Project tooling: the privacy check covers more encodings and also author and committer; the
  tests that launch copies of the program only run in CI.

## [0.1.3] - 2026-10-16

Third round of cleaner fixes, after a new external audit of 0.1.2. **Update if you use the
cleaner.** Disk scanning does not change. It is still **unsigned**: Windows SmartScreen may warn.

### Security

- Each folder is sent to the Recycle Bin **on its own**, measured and checked against the available space right before. Files
  are still sent together, but Windows checks the size of each one again at the last moment and, if
  it grew, it is not sent.
- If Windows returns an error in the middle of a send, the rest of that batch is no longer processed.
- The log of a cleanup in progress can no longer be deleted while it is open (for example, by another
  simultaneous cleanup).
- When Tesela cannot check what happened to a folder, it says so ("could not be checked") instead of treating it as
  deleted; and when a folder changed while Windows was processing it, it says it was deleted or changed, without claiming more
  than it knows. In both cases sending stops.

### Changed

- A basket with many folders takes a little longer to send to the Recycle Bin (each folder goes in its own operation).

## [0.1.2] - 2026-10-16

Second round of fixes after two external audits of 0.1.1 (one of the cleaner and one of the rest).
**Update if you use the cleaner.** Disk scanning does not change in essence. It is still **unsigned**: Windows
SmartScreen may warn.

### Security

- Stricter sending to the Recycle Bin: each file and folder is measured again right before it is sent, and whether it
  fits is recalculated. What grew since the confirmation, or what could not be measured in full, is not sent.
- If Windows fails after having deleted part of something, it is counted as deleted and sending stops. A folder
  is only considered intact if its contents match (a fingerprint of each item, not just the total size).
- Emptying the Recycle Bin checks the identity of each item at the last moment. A substitute is vetoed and what came
  after it is not touched.
- **It does not clean when running as administrator** (for example, opened from an elevated console). Scanning and
  simulating still work.
- Hardening against planted DLLs: the program only looks for its DLLs in System32 and no longer tries to load
  `dxcompiler.dll`.
- The scanner no longer follows a link (junction or symlink) chosen as the root folder: it rejects it with a clear message.
- The local log writes each result at the moment it happens, and its pruning can no longer delete the log that was just opened.
- Demo mode (`--demo`) and the measurement tools no longer read or save the configuration in your profile.

### Changed

- Texts that do not overpromise: "Up to X in low-risk candidates (checked when cleaning)", "Free up to X" and
  "Measured change in free space" (other programs also change it).
- Temporary files are cleaned if they have gone **more than 7 days** unmodified (before, 24 h), as Windows does.
- The `hiberfil.sys` note explains that a command prompt as administrator is needed, what is lost with
  `powercfg /h off` and the alternative `powercfg /h /type reduced`.
- A folder with unreadable parts is rejected on approval, with the reason.
- New tagline: *"See what takes up your disk and decide what to free, knowing what gets deleted and checking what was actually freed."*
- Project tooling: the privacy check detects more path forms and split values; the SHA-512 of
  WiX is checked before installing it; the MSI validator and the tests are stricter.
- New product roadmap in `docs/ROADMAP.md`.

## [0.1.1] - 2026-10-16

Cleaner security fixes after an external audit of 0.1.0. **Update if you use the cleaner**: disk
scanning (map, list and details) does not change. It is still **unsigned**: Windows SmartScreen may warn.

### Security

- A *junction* above a cache or temporary folder can no longer make a protected folder deletable. The
  path of each category is checked piece by piece, accepting no link, and each exception only lifts its own
  zone. If a category is discarded because of that (for example, the browser folder moved to another disk with a link),
  Recommendations shows it as "not available" with the reason.
- Sending to the Recycle Bin fails closed: if it is not known how much fits, it is not sent. Folders are measured again right
  before being sent, and if they grew since the confirmation they are not sent.
- Right before Windows acts, it is checked that each item is still the approved one; a substitute with the same
  name is not sent. Sending stops as soon as Windows permanently deletes something, not at the end of the batch, and it no
  longer drags along associated files (the `_files` folder of a saved page).
- If Windows says it deleted something, Tesela checks it against the object itself and its contents: a real deletion,
  even a partial one, is never recorded as "nothing was touched".
- Emptying the Recycle Bin checks the identity of each item before purging it and only cleans orphaned metadata with
  complete evidence.
- A file that gets a hard link added or removed after the confirmation is rejected.

### Changed

- **No log, no cleanup**: if the local log cannot be created or written, the cleanup does not start or
  stops. Before each operation what is about to be done is recorded, and emptying the Recycle Bin records each item.
- On a disk **with no maximum Recycle Bin size configured**, what would go to the Recycle Bin moves to the permanent
  deletion group, which only runs if you explicitly check it; the confirmation explains how to set that size.
- Items that changed since the confirmation show as "changed since the confirmation" and are not touched.
- A simulation (`--dry-run`) no longer deletes logs of real cleanups; each class of log has its own limit.
- `leak-scan` and `privacy-check` also detect names with non-ASCII letters (UTF-8, Latin-1 and UTF-16 without BOM), and the
  MSI validator checks the exact folder tree, upgrade rules and their order.
- Third-party notices regenerated: `getrandom` 0.4 was missing.

## [0.1.0] - 2026-10-16

First version (MVP). **Unsigned**: Windows SmartScreen may warn (see the README, "Installation").

### Added

- Read-only **scanner** of drives and folders (parallel walker with real `AllocationSize`, hard links without
  overcounting, cloud folders without downloading), and the `tesela-scan` console tool.
- **Interface**: synchronized disk map (treemap) and list, details, scan screen with real progress and an
  early verdict, Spanish and English, light and dark theme, accessibility (per-monitor DPI, visible focus, AccessKit in the
  list) and `--demo` mode with synthetic data.
- **Safe cleaning**: Recommendations (temporary files, browser caches, Downloads and Recycle Bin), review basket,
  confirmation grouped by destination and a summary with what was really freed. Deletion by handle without following reparse points,
  sending to the Recycle Bin verified by identity, protection of system folders, simulation (`--dry-run`) and a log
  of every cleanup on your computer.
- **Per-user MSI installer** (WiX 5.0.2): no administrator, in `%LOCALAPPDATA%\Programs\Tesela`, with a Start
  menu shortcut and an Installed apps entry; a new version replaces the previous one and an older one is
  blocked. On uninstall your configuration and your history are **not** deleted. **Portable zip** and `SHA256SUMS.txt`.
- **Automated releases**: pushing a `vX.Y.Z` tag builds the MSI and the zip, attests their provenance
  (`gh attestation verify`) and creates a GitHub Release as a **draft**, which a person publishes after reviewing it.
- Tile icon and version data (product, publisher) in `tesela.exe` and `tesela-scan.exe`.

### Changed

- The `cleaner` tests that use the real Recycle Bin only run with `TESELA_REAL_RECYCLE_TESTS=1` (GitHub's CI
  has no reliable user Recycle Bin).
