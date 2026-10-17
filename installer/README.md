# Tesela MSI installer

`Tesela.wxs` (the package) and `TeselaUI.wxs` (the interface) make up the Tesela MSI installer: **with a user interface**, for
**the current user only (without administrator access) or all users** (PLAN.md §1 and §3.11, round 14). They are compiled
together with [`scripts/build-msi.ps1`](../scripts/build-msi.ps1), which always starts from the output of
`scripts/build-release.ps1`.

## Tool

**WiX Toolset 5.0.2**, the `wix` CLI as a global `dotnet tool`. License: **MS-RL**; it is a build tool only, and no WiX
components are redistributed in the MSI.

```powershell
pwsh -File scripts/install-wix.ps1 -Version 5.0.2    # install (and verify the package's SHA-512)
dotnet tool uninstall --global wix                   # remove
```

`install-wix.ps1` runs `dotnet tool install --global wix --version 5.0.2 --configfile installer/nuget.config`: that
`nuget.config` (`<clear/>` and nuget.org only) makes dotnet ignore every other NuGet source on the machine, after which the
SHA-512 of the installed `.nupkg` is compared with the value published by nuget.org (pinned in the script).

The version is pinned in `scripts/build-msi.ps1`, `scripts/install-wix.ps1` (along with its SHA-512),
`.github/workflows/ci.yml`, and `.github/workflows/release.yml` (`scripts/tests/wix-version.tests.ps1` checks they match).
**Why 5.0.2 instead of 6.x or 7.x:** since 6.0, the official binary has been distributed under the OSMF (*Open Source
Maintenance Fee*; charged only to those earning revenue with WiX, while the code remains MS-RL), and the tool asks users to
**accept** it. Version 5.0.2 is pure MS-RL and asks for no acceptance. Upgrading is a deliberate decision: it changes
everything at once, so the full workflow must be tested again. **`WixToolset.UI.wixext` is not used** (`WixUI_*` dialogs):
its custom actions include a DLL and conflict with the rule against shipping custom code; the interface is written by hand in
`TeselaUI.wxs`.

## What it installs

- `tesela.exe`, `tesela-scan.exe`, `LICENSE-MIT`, `LICENSE-APACHE`, and `THIRD-PARTY-NOTICES.md` in the selected folder.
  Defaults: **current user only** in `%LOCALAPPDATA%\Programs\Tesela` (no administrator access), or **all users** in
  `C:\Program Files\Tesela` (administrator access required).
- A **"Tesela"** shortcut in the Start menu (the user's menu or the computer-wide menu for all users). A **desktop shortcut**
  is created only when requested (interface checkbox or `DESKTOPSHORTCUT=1`); it is remembered during upgrades and removed on
  uninstall. No changes to `PATH`.
- An **Installed apps** entry (icon, publisher "Tesela contributors", version, and repository link). Windows Installer writes
  its uninstall key itself (under `HKCU` or `HKLM`, depending on the context; without requesting elevation for a per-user
  installation); the icon is provided through `ProductIcon` in `%APPDATA%\Microsoft\Installer\{ProductCode}`.
  `ARPINSTALLLOCATION` is left empty: filling it would require a custom action.
- Values under `Software\Tesela` in `HKCU` (per-user) or `HKLM` (all users): one per component (Windows Installer needs these
  as *KeyPaths*), plus `InstallFolder` and, when there is a desktop shortcut, `DesktopShortcut`. They are removed on uninstall.
- **No custom code:** the only custom actions are **type 51** (set a property: folder, preselected scope, language, or Spanish
  text); there are no DLLs, executables, or `Binary` table. `msi-validate.ps1` compares the `CustomAction` table row by row.

## The interface (`TeselaUI.wxs`)

Welcome, **Scope**, **Folder and options**, Ready, Progress, and Exit, plus Maintenance (open the already-installed MSI to
repair or uninstall), Browse, and the required Windows Installer dialogs (error, cancellation, interruption, files in use,
and waiting for space calculation). No images or icons (`Binary` is prohibited). Only **native** control events
(`NewDialog`, `SpawnDialog`, `EndDialog`, `SetTargetPath`, `Reinstall`, `Remove`...); Browse is the native dialog (`PathEdit`,
`DirectoryCombo`, and `DirectoryList`), and the folder is validated with `SetTargetPath` and conditions on the property.

- **Scope.** "Current user only (recommended)" or "All users of this computer." The text states plainly: *installing for
  everyone does NOT give Tesela more permission to clean; Tesela always runs with normal permissions and refuses to clean
  when run as administrator (security decision E1).* The package is **dual-purpose** (`Scope="perUserOrMachine"`:
  `ALLUSERS=2` and `MSIINSTALLPERUSER=1`) and is **not** declared "UAC compliant" (bit 8 of the Summary Information word
  count, which the validator requires to be CLEAR): with that bit set, Windows Installer ignores `MSIINSTALLPERUSER` and
  "all users" would not request credentials (error 1925). Without it, opening the MSI asks for nothing, "current user only"
  never elevates, and "all users" requests administrator access **after** the interface, when installation starts (the
  Install button shows the UAC shield when "all users" is selected). Microsoft documents this last behavior, but it has
  **not been checked on a real machine** (CI has no desktop on which to show the prompt); a person must verify it.
- **Folder.** It changes with the scope unless the user has already customized it. During an upgrade, the existing folder is
  retained and cannot be changed (field and button disabled, with a note). If a protected folder (Program Files, Windows) is
  selected for "current user only," a warning appears in the dialog and Next opens an "invalid folder" dialog instead of
  allowing the install to fail later with an unexplained 1603 (even for an administrator: the right choice is "all users").
- **Desktop.** "Create a desktop shortcut" checkbox, cleared by default (`DESKTOPSHORTCUT`).
- **Pin to taskbar: unavailable.** Windows prohibits installers from doing this. The final screen says: *To pin Tesela to the
  taskbar: open the Start menu, right-click Tesela, and select Pin to taskbar.*
- **Language.** A single MSI. English texts are ordinary properties; if the Windows user language (`UserLanguageID`) is any
  Spanish variant, a type 51 action per text (only in the UI sequence) replaces them with Spanish. `TESELA_LANG=es` or
  `TESELA_LANG=en` on the command line forces the language (useful for testing). `UserLanguageID` is the language Windows
  assigns to the user (regional format): Windows with an English display language and Spanish regional format shows Spanish.
  **The `ActionText` and `UIText` tables (progress-bar text) cannot be selected by language without code**; they show Spanish
  and English together ("Copiando archivos / Copying files"). **The installer follows Windows' regional format, not its
  display language** (Windows Installer does not expose the display language without code; the Tesela application does use
  it): with Spanish display language and English regional format, the installer is in English and the application in Spanish,
  and vice versa. This is accepted; `TESELA_LANG=es|en` forces the language. Generic Windows Installer errors use the Windows
  language. Launch-condition messages in silent mode also show both languages together.

  Early launch-condition errors can appear before the Spanish UI properties are initialized: their buttons remain in English.
  The condition message itself is bilingual. This cosmetic limitation is accepted to keep localization out of the early
  installer validation sequence; it does not change the condition or installation scope.

### Silent mode (`/qn`)

Same as v0.1.4: **per-user and no UAC by default**.

| Property | Effect |
|---|---|
| (none) | Per-user, `%LOCALAPPDATA%\Programs\Tesela`, no desktop shortcut. |
| `ALLUSERS=1` | All users (administrator required): `C:\Program Files\Tesela` and common Start menu. This is the **only** documented way to request it. |
| `INSTALLFOLDER=<folder>\` | Another folder (see below). |
| `DESKTOPSHORTCUT=1` | Creates a desktop shortcut (`0` removes it during upgrade; empty keeps it if it already existed). |
| `TESELA_LANG=es\|en` | Affects the interface only. |

### Scope and upgrades

Windows Installer **does not upgrade across contexts**: a per-user product cannot be replaced by an all-users installation,
or vice versa. A previous installation is **detected** (a key with `InstallFolder` **and** `tesela.exe` in that folder; a key
without that executable does not count; `HKCU\Software\Tesela\InstallFolder`, already written by v0.1.4, and
`HKLM\SOFTWARE\Tesela\InstallFolder`) and **scope changes are blocked** with a message saying to uninstall first.
**Exception (multiple accounts):** if THIS user has their own per-user installation, it takes precedence and can be upgraded
even if another account installed for all users (two "Tesela" shortcuts will be visible); the reverse, requesting all users
when this user has a per-user installation, remains blocked. **Decision and reason:** blocking is preferred to silently
*fixing* the scope. In silent mode (`/qn`) there is no one to explain this to, and silently fixing it would change the
command-line request; in the interface, the scope dialog **preselects** the existing installation's scope (upgrade means
pressing Next), and selecting the other scope shows a warning and disables Next. In silent mode, an upgrade of an all-users
installation must be run with `ALLUSERS=1`.

- v0.1.4 (per-user) → v0.1.5 per-user: upgrades in place, preserves a custom folder, and leaves one product.
- v0.1.4 (per-user) → all users: blocked; uninstall first.
- Downgrades remain blocked.

#### Recovering orphaned installer metadata

The executable check does **not** verify Windows Installer product registration. If both `InstallFolder` and an orphaned
`tesela.exe` remain after product registration is removed, setup can still ask you to uninstall first. Check Settings > Apps >
Installed apps for Tesela, in the affected user account and for the all-users installation. If a registered installation
exists, uninstall it normally; do not edit its metadata. Only after confirming that the affected scope has no registered
Tesela installation, export its `Software\Tesela` registry key as a backup and remove **only its `InstallFolder` value**, then
run setup again. Use HKCU for the affected user or the 64-bit HKLM view for all users (administrator permission required).
Leave orphaned files in place until you have reviewed them; clearing this metadata does not uninstall or delete those files.
If registration state is uncertain, repair or uninstall with the original MSI rather than deleting metadata.

### Custom folder

`msiexec /i tesela-<version>-x64.msi INSTALLFOLDER="%LOCALAPPDATA%\My programs\Tesela\"` installs there (in `cmd`,
`%LOCALAPPDATA%` is expanded by the command interpreter). **Choose a folder that only you can write to** (see the next
section). The folder is **remembered** in `Software\Tesela\InstallFolder` (HKCU or HKLM depending on scope; a
`RegistryValue` containing `[INSTALLFOLDER]`) and **retrieved** with a registry search (`TESELA_PREV_USER` /
`TESELA_PREV_MACHINE`) that also runs during upgrade, repair, and uninstall. This lets `msiexec /x` (or Installed apps)
remove files from a custom folder without repeating the parameter, and keeps upgrades in the same location. As a consequence,
**you cannot change the folder of an existing installation by passing a different `INSTALLFOLDER`**; uninstall and reinstall
instead. Without this, files from a custom installation would be left behind on uninstall.

**Do not edit `Software\Tesela\InstallFolder`:** the installer manages it. If changed manually, the installer does not
reliably treat it as the source of truth or remove it on uninstall (an installation with a changed value can leave files or
the key itself behind). To move the installation, uninstall and reinstall with `INSTALLFOLDER`.

### Secure properties and `AlwaysInstallElevated` (reverses v0.1.4 decision R-S3)

`INSTALLFOLDER` and `DESKTOPSHORTCUT` are now **secure properties** (`ALLUSERS` and `MSIINSTALLPERUSER` do not need to be
marked: `ALLUSERS` is always passed to the server and `MSIINSTALLPERUSER` is read only with `ALLUSERS=2`)
(`SecureCustomProperties`; `INSTALLFOLDER` and `DESKTOPSHORTCUT` plus the two `MajorUpgrade` properties). v0.1.4 had no
interface and was per-user only, so there was nothing to pass to the server and `Secure` was omitted. Now the interface sets
the folder and shortcut in the UI phase; in an elevated **all-users** installation, Windows Installer passes only secure
properties to the server. Without `Secure`, "all users" would silently ignore the selected options. **Risk with
`AlwaysInstallElevated`** (HKLM and HKCU policy, disabled by default): any user can install **any** `.msi` with SYSTEM
permissions (the escalation comes from the policy, not the package), and `Secure` also lets the user choose the folder where
the server writes. **This is accepted** because it adds nothing: the MSI fixes the contents (five Tesela files), with that
policy the user could already install their own MSI as SYSTEM, `RemoveFolder` removes only empty folders, and the protected
folder list only displays a warning. The alternative (a custom action that rereads the value) is prohibited by the no-custom-
code rule. `msi-validate.ps1` requires these properties (and only these) in `SecureCustomProperties`.

### Installation folder security (`INSTALLFOLDER`)

The MSI **accepts any folder** for `INSTALLFOLDER`, including one writable by other users on the computer (a drive root, a
shared folder, `C:\Users\Public`...). For a per-user install, the installer does not elevate and can write only where that
account already has write access (for "all users," it does elevate and writes to the `INSTALLFOLDER` location; see Secure
properties). The risk lies with whoever chooses the folder: if **another user can write to it**, they could place there a DLL
named after one that `tesela.exe` loads by name, and it would be loaded with the permissions of whoever runs Tesela. The DLLs
the program loads by name (before the hardening described below; reviewed in the source for `wgpu-hal` 30.0.1 and
`libloading` 0.8.9, which uses `LoadLibraryExW(name, NULL, 0)`: the **default** search order, with the `.exe` folder first
and without `LOAD_LIBRARY_SEARCH_SYSTEM32`) are `d3d12.dll`, `dxgi.dll`, `dcomp.dll`, `d3dcompiler_47.dll`, and
**`dxcompiler.dll`**; none is in Windows `KnownDLLs`. The most important one is `dxcompiler.dll`: until 0.1.2, `tesela.exe`
did not pin the DX12 shader compiler, and `wgpu` defaults to `Dx12Compiler::Auto`, which **tries to load `dxcompiler.dll`
by name before falling back to `d3dcompiler_47.dll`**. That DLL **does not exist in a standard Windows installation**, so a
copy placed beside the `.exe` (or in a `PATH` folder) does not compete with one in `System32` (DXC also loads `dxil.dll`).
The `WGPU_DX12_COMPILER` environment variable also changes this choice. `dbghelp.dll` is not loaded by `wgpu`: the Rust
standard library loads it, if needed, when generating a *backtrace* (the latter has not been checked in Rust source).
**This is a suspected risk, not one demonstrated at runtime.**

**Hardening in the program itself** (`tesela.exe` and `tesela-scan.exe`) reduces the risk even if others can write to the
folder, but does not replace the recommendation below:

- `main` first calls `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` (the `dll_hardening` module): later loads by
  name (`d3d12.dll`, `dxgi.dll`, `dcomp.dll`, `d3dcompiler_47.dll`...) search only `System32`, never beside the `.exe`.
- The DX12 shader compiler is pinned to FXC (`d3dcompiler_47.dll`, from the system): `dxcompiler.dll` is no longer probed,
  and `WGPU_DX12_COMPILER` cannot change this.
- The `.exe`'s **static** imports (`dxgi.dll`, `dwmapi.dll`, `uxtheme.dll`, `propsys.dll`, `uiautomationcore.dll`,
  `bcryptprimitives.dll`, and `winhttp.dll` for optional update checks even while disabled) are resolved by the loader before
  `main`, so `SetDefaultDllDirectories` does not cover them. They
  are linked with `/DEPENDENTLOADFLAG:0x800` (`build.rs`), which searches only `System32`. Tested with empty files bearing
  those names beside a copy of the `.exe`: without hardening, the process does not start (a fake `dxgi.dll` is enough); with
  hardening it starts and renders, and all those modules come from `System32`.
- Limits: this does not protect against anything already loaded by the loader before `main` by other means, DLLs Windows
  itself loads with other flags (for example, `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`, which includes the application folder and
  is used by `windows-core` to activate WinRT classes that `combase` does not resolve), or a replaced `.exe`. GPU driver DLLs
  are loaded by absolute path and are unaffected.

**Decision: no MSI guard is added; the behavior is documented, with a warning only for obvious cases.** A `LaunchCondition`
rejecting an `INSTALLFOLDER` outside the profile was evaluated, but cannot be made robustly (the MSI already uses type 51
actions, but that does not change the fundamental issue):

- A text comparison cannot detect `..` (`<profile>\..` begins with the profile), a junction inside the profile that points
  outside, redirected profiles, or 8.3 short names; it would also reject legitimate installations in a user-owned folder
  outside the profile (on another drive).
- Without a trailing slash, a profile-prefix check would also accept a similarly named sibling path, such as the current profile path with `2` appended, which could refer to another user.
- And **this cannot be checked here**: validating the condition requires running `msiexec /i`, which local agents do not do
  (installation tests run in CI: `scripts/tests/msi-install.tests.ps1`).

What is provided, **only to show a readable message and not as a guard**: for "current user only," a folder under Program
Files or Windows (case-insensitive prefix comparison without a trailing slash; covers `Program Files`, `Program Files (x86)`,
and `Program Files (Arm)` on ARM64, and also treats names beginning the same way, such as `Program Files2`, as protected)
shows a warning in the dialog (and, with `/qn`, a launch-condition message suggesting `ALLUSERS=1`) instead of failing with
an unexplained 1603. This does not depend on `Privileged`: Windows Installer documentation says it has the same value as
`AdminUser` (the user is an administrator), not whether the process is elevated; an unelevated administrator also cannot
write to Program Files. Path formats that bypass the warning are also rejected: it must be an absolute drive path
(`C:\...`) without `/`, `..`, `?`, or a network path (the dialog shows "invalid folder"; `/qn` uses a launch condition).
**8.3 short names** (`C:\PROGRA~1`) and a link inside the profile that points outside are **not detected**: without code,
there is no way to do so; this is a warning, not a guard.

What to do: install in the default folder (`%LOCALAPPDATA%\Programs\Tesela`, which belongs to you) or another folder **that
only your account (and administrators) can write to**. To check, `icacls "<folder>"` should not show `Users`, `Everyone`, or
`Authenticated Users` with write permissions (`W`, `M`, `F`, `(OI)(CI)(W)`...). The same applies to the **portable zip**:
extract it into a folder in your profile, not a shared folder or a drive root. For a company installation, strengthen this
with the folder's NTFS permissions.

## Identifiers (DO NOT change)

| What | Value | Why |
|---|---|---|
| `UpgradeCode` | `{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}` | Identifies Tesela as the product **forever**. With a different value, a new version would not recognize the previous one and both would remain installed. `build-msi.ps1` fails if it changes. |
| GUID for each component | Fixed in `Tesela.wxs` (7: the 6 from v0.1.4 and the desktop shortcut) | Windows Installer identifies components across versions by their GUID. |
| `ProductCode` | New on every build (`Id="*"`) | Required by `MajorUpgrade`: each version is a distinct product that replaces the previous one. |

## Upgrade, repair, uninstall

- **Upgrade:** `MajorUpgrade` uninstalls the previous version and installs the new one (no duplicates, in the same folder and
  scope). `AllowSameVersionUpgrades` also allows installing a build of the *same* version over itself. Installing an **older**
  version is blocked with the `DowngradeErrorMessage` text (error 1603 in the `msiexec` log). Scope changes are blocked (see above).
- **Repair:** `msiexec /fa` (or "Repair" when opening the already-installed MSI) restores missing or modified files.
- **Uninstall:** removes the folder, shortcuts (Start menu and desktop), Installed apps entry, and `Software\Tesela` values
  (HKCU or HKLM). It leaves the parent folder (for example `%LOCALAPPDATA%\Programs`): it is no longer below a known profile
  folder, so `RemoveFolder` removes only the Tesela folder, and only if it is empty. It does **not** delete
  `%APPDATA%\Tesela\config.toml` or `%LOCALAPPDATA%\Tesela` (history and cleanup logs): the installer does not know about
  them; they belong to the user. To delete them manually, see the root README ("Installation").
- **App open during an upgrade:** Windows Installer uses Restart Manager to ask it to close (the interface includes the
  `FilesInUse` and `MsiRMFilesInUse` dialogs); if it cannot (or the app refuses: Tesela prevents closing while cleanup is
  running), files in use are replaced at restart. *Not tested on a real machine*; this is expected Windows Installer
  behavior, not behavior specific to Tesela.
- **Two users on the same computer:** with per-user installs, each account has its own folder and Start menu; they do not
  interfere or request administrator access. With an all-users install, one installation serves every account. *Not tested*
  (a second account would be needed).

## Build and validate

Requirements: Windows with the MSVC toolchain and the **Windows SDK** (`rc.exe`, used by `winresource` for the icon and
version data), Python 3, and WiX 5.0.2 (above).

```powershell
pwsh -File scripts/build-msi.ps1
```

This builds the executables (`build-release.ps1`: static CRT, `--locked`, reproducible linking, no machine paths, PE imports),
packages the MSI (both `.wxs` files) and portable zip, writes `SHA256SUMS.txt`, and **validates** the MSI
(`scripts/msi-validate.ps1`): ICE (only exact ICE61 messages and the two ICE57 messages for shortcut components are accepted;
their KeyPath is an HKMU value, the standard design for a dual-purpose package), a **closed** table allowlist (installer and
UI tables; `Environment`, `ServiceInstall`, `Class`, `Extension`, `ProgId`, `Binary`... fail), the `CustomAction` table
**row by row** (types 51 and 35 only), dual scope (`ALLUSERS=2`, `MSIINSTALLPERUSER=1`, and the "no elevation required" bit
CLEAR), the **exact directory tree** (`TARGETDIR` > `INSTALLFOLDER`, plus `ProgramMenuFolder` and `DesktopFolder` under
`TARGETDIR`), the 8 HKMU registry values, six searches for previous installations (four values and the `tesela.exe` check),
the **two exact `Upgrade` rows**, the **five `LaunchCondition` rows** (later versions, scope changes in both directions,
protected folder, and path format), exact secure properties, sequences (`FindRelatedProducts` before `LaunchConditions`,
`RemoveExistingProducts` between `InstallValidate` and `InstallInitialize`, and folder/language/desktop actions with their
conditions and sequence numbers), the **interface** (expected dialogs, controls without images only, native events only,
scope and folder dialogs row by row, and every text in English **and** Spanish), files byte for byte against the compiled
files, and that neither the MSI nor the zip or their contents contain machine data (identifiers are compared case-sensitively).
Output goes to `<cargo target>/dist`; nothing is written to the repository.
`scripts/tests/msi-validate.tests.ps1 -Msi <msi>` tests the validator with modified copies of the MSI (without installing them).
`scripts/tests/msi-logic.tests.ps1 -Msi <msi>` tests **logic** (launch conditions, folder, scope, language, desktop shortcut,
and Next buttons in the scope and folder dialogs) with a Windows Installer **session**, without installing anything
(`OpenPackage` with synthetic properties: Windows Installer's own condition evaluator is used, and it never reads the
registry or real folders). It can be run locally without changing the machine.

**Installation tests run only in CI** (`scripts/tests/msi-install.tests.ps1`, the `msi` job in `ci.yml`: the runner is a
disposable administrator, and the script refuses to run outside a GitHub runner). They download the published v0.1.4,
verify it with `gh attestation verify` and its SHA-256, and test in silent mode: per-user install, repair, and clean uninstall;
upgrade from v0.1.4 (default and custom folders, one product, downgrade still blocked); all-users install (Program Files,
HKLM, common Start menu, custom folder, upgrade, and clean uninstall); scope changes blocked in both directions;
`DESKTOPSHORTCUT` (per-user and all-users, preserved on upgrade, removed on uninstall); protected folder; registry key without
a product; and a disposable **standard user** (random password generated in the job and never printed, account removed at the
end even on failure). `ALLUSERS=1 /qn` without privileges must fail without leaving anything; per-user installation without
elevation must finish with code 0 in that user's profile without touching Program Files or HKLM; and that per-user
installation can be upgraded even if another account installed for all users (M2). **The runner is Windows Server**, where
the effective Windows Installer policy is `DisableMsi=1`: a standard user cannot install any MSI (error 1625, regardless of
package, before scope is decided). To test the relevant behavior on client Windows (default value 0), the disposable machine
gets `DisableMSI=0` for that scenario and the policy is removed afterwards. With it, the msiexec log says "dual mode package
... installed in Per-User mode. No elevation is required"; without elevation, `ALLUSERS=1 /qn` exits with 1603 and installs
nothing. The job summary lists each check, skipped checks (not counted as passed, and failing the job if the published
version is already newer than v0.1.4), and `MSI_LUA` lines from msiexec logs (how the engine decided scope and elevation).
**No agent or CI run verifies:** the interface (appearance, text, flow), the administrator prompt (UAC) after clicking
Install for "all users" (the runner has no interactive desktop to show it), interface language on Spanish Windows, secure
properties when a **non-admin** supplies another administrator's credentials, or the folder dialog's protected-folder
warning. A person must check these, with the user informed.

The icon is **not versioned** (`privacy-check` rejects images): `crates/app/build.rs` draws it on each build and embeds it in
`tesela.exe`; `scripts/extract-ico.py` extracts it from the executable for the MSI. (Pointing the `Icon` table at the `.exe`
would put a complete copy of the executable in the MSI.) The interface has no images either.

The build is **reproducible**: `-Clink-arg=/Brepro` and `--remap-path-prefix` make the same commit produce the same `.exe`
byte for byte in different folders. (The MSI and zip are not reproducible: they contain timestamps, and `ProductCode` is new
on each build.)

## Before the first release

**[TO BE CONFIGURED BY THE MAINTAINER]** These repository settings have **not yet been configured** (they cannot be set in
code and CI does not configure them). Until then, anyone with write access can push to `main` or create a `v*` tag that
triggers `release.yml`:

1. **`v*` tag ruleset:** administrators only, with no force-push or deletion.
2. **`main` protection:** require a pull request, code owner review (`.github/CODEOWNERS`), and the required CI checks
   (`privacy`, `privacy-history`, `build`, `deny`, `msrv`, `aarch64`, `msi`).
3. **`release` environment** with a required reviewer.
4. **"Require SHA pinning"** for actions (Settings > Actions).
5. Immutable releases.
6. **MFA** on all accounts with access (GitHub and, when available, SignPath).
7. Enable **"Private vulnerability reporting"** (Settings > Code security) so the "Report a vulnerability" button in
   `SECURITY.md` works (it is currently disabled; until then, use the fallback channel described there).
8. (Optional) Rehearse the workflow: Actions > Release > Run workflow on `main` (a rehearsal; it publishes nothing).
9. Date the section in `CHANGELOG.md` and check that the tag matches the version in `Cargo.toml`.
