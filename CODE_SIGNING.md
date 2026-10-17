# Code signing policy

> **Status: v0.1 is published UNSIGNED.** This document is the policy that [SignPath Foundation](https://signpath.org/) asks for
> in its [terms](https://signpath.org/terms) for its free program for open source projects; it is linked from the README as
> **"Code signing policy"** (top and downloads section) and applies from the moment the project is accepted (the application is
> submitted by the maintainer, not by CI). Meanwhile, the integrity of the downloads is checked with `SHA256SUMS.txt` and with
> GitHub's provenance attestation (README, "Installation").
>
> Markers: **[TO COMPLETE]** and **[TO CONFIRM]** are decisions or checks for the maintainer before submitting the application;
> **[TO BE CONFIGURED BY THE MAINTAINER]** are repository settings that are **NOT done** today and cannot be done from the
> code (the maintainer decides them). This text is a reasonable proposal, not a verified fact.

## Attribution

Once there is signing, this sentence will be shown (README and downloads page), exactly as SignPath requires:

> Free code signing provided by [SignPath.io](https://about.signpath.io/), certificate by
> [SignPath Foundation](https://signpath.org/).

## What is signed

Only artifacts **built by this repository's release workflow** from the public source code, without signing
anything compiled outside it:

| Artifact | Signature |
|---|---|
| `tesela.exe`, `tesela-scan.exe` | Authenticode |
| `tesela-<version>-x64.msi` (per-user installer) | Authenticode |
| `tesela-<version>-x64-portable.zip`, `SHA256SUMS.txt` | Not signed (the zip contains the same already signed `.exe` files; the sums and GitHub's attestation cover the rest) |

The executables carry the product attributes SignPath requires: product "Tesela", publisher "Tesela contributors" and the
version from `Cargo.toml` (`VERSIONINFO` resource, the same one in the MSI). **Planned order:** sign the two `.exe` files first,
rebuild the MSI with them and then sign the MSI. Today `build-msi.ps1` packages what comes out of `build-release.ps1`;
reordering it to sign between the two steps is pending work of the onboarding process (the step is prepared, disabled, in
`.github/workflows/release.yml`; there it is also noted that a v3 of the SignPath action exists).

No third-party dependency is signed separately: they are compiled into the executable and their licenses are in
`THIRD-PARTY-NOTICES.md`.

## Build provenance

- Releases come **only** from `.github/workflows/release.yml`, which is triggered by pushing a `vX.Y.Z` tag of a commit
  on `main`, on a GitHub-hosted runner (`windows-latest`). Nothing is built or signed on personal machines.
- The build is that of `scripts/build-release.ps1` (no machine paths, static C runtime, `--locked`, reproducible linking,
  exact set of executables, imports check) and `scripts/build-msi.ps1` (validated per-user MSI, byte for byte
  against what was compiled). GitHub Actions are pinned by SHA and WiX by version and SHA-512.
- Each release carries a **provenance attestation** (SLSA, `actions/attest-build-provenance`) that is verified with
  `gh attestation verify <file> --repo wandoth1/tesela --signer-workflow wandoth1/tesela/.github/workflows/release.yml`;
  the workflow itself verifies it before creating the release.
- The workflow creates the GitHub Release as a **draft**; nothing is published without a person.

## Roles and approval

The three SignPath roles (see their [terms](https://signpath.org/terms)):

| Role | What it means | Who |
|---|---|---|
| **Authors** | Trusted to modify the code without additional review | `@wandoth1` **[TO COMPLETE: list of authors with write permission; today only the owner]** |
| **Reviewers** | Review every change from someone who is not an author (pull requests) | **[TO COMPLETE: who reviews; today, the owner]** |
| **Approvers** | Approve **each signing request**: they decide whether a release can be signed | `@wandoth1` **[TO COMPLETE: name and SignPath account]** |

- **Each release is approved by hand** in SignPath: the production signing policy requires manual approval, and the request
  is only accepted if it comes from this repository's `release.yml` workflow on a tag of `main` (verification of the
  origin of the build that SignPath does with its GitHub connector). The same approver then publishes the release draft.
- **[TO CONFIRM with SignPath]** If the project stays with a single person, that person is at once author, reviewer and approver
  (with explicit manual approval and MFA); it must be checked that SignPath Foundation allows this for a one-person project.
  If another member joins, the reasonable setup is for them to review the changes and approve the signatures.
- **MFA required** for all members, **both in SignPath and in the code repository** (GitHub), as SignPath's terms
  require. **[TO BE CONFIGURED BY THE MAINTAINER: enable and check MFA on the GitHub account and the SignPath account
  before the application.]**
- The SignPath secrets (token and organization) will live only in the GitHub repository secrets and are not in the code.

### Repository protection: NOT CONFIGURED TODAY

Today there is **no protection of `main`** nor of the tags: anyone with write permission can push to `main` or create a `v*`
tag, and that would trigger the release workflow. The workflow has been hardened internally (the tag has to be from `main` and
match `Cargo.toml`, minimal permissions per job, draft, verified attestation), but **those are checks INSIDE the workflow itself, not security
controls**: whoever pushes the tag (and has write permission) can edit the workflow in that same tag and skip them. What really prevents it is
what follows, which belongs to the repository, not to the code, and **must be configured before the first release and before requesting signing**:

**[TO BE CONFIGURED BY THE MAINTAINER]**
1. Ruleset for `v*` tags: only administrators can create them, no force-push or deletion.
2. Protection of `main`: mandatory pull request, review by the code owners (`.github/CODEOWNERS`) and the CI checks
   (`privacy`, `privacy-history`, `build`, `deny`, `msrv`, `aarch64`, `msi`) as required.
3. `release` environment with a required reviewer (and use it in the `release`/`attest` job when it exists).
4. "Require SHA pinning" for actions (Settings > Actions).
5. Immutable releases (a published release cannot be modified or replaced).
6. MFA on all accounts with access (above).

The same list appears in `installer/README.md`, section "Antes de la primera release" (before the first release).

## Privacy policy

This program will not transfer any information to other networked systems unless specifically requested by the user or the
person installing or operating it.

Tesela has no telemetry or accounts; it scans only the drive or folder you choose and keeps
its settings and the cleanup log **only on your computer** (`%APPDATA%\Tesela` and `%LOCALAPPDATA%\Tesela`).
It makes **no network connection by default**. The only request it can make is the **optional update check**
(*Settings > Check for updates when Tesela opens*, off by default): when the box is ticked and then at most once every 24
hours it asks GitHub (`api.github.com`, through WinHTTP) for the latest published version, sending only the fixed
`User-Agent` `Tesela` (no version, system, identifier or credentials), downloads only that answer (never an update, and
nothing is installed), and never runs in `--demo` or in the measuring tools. Through WinHTTP it uses the system proxy and,
on networks that enable it, may discover the proxy configuration automatically (WPAD), which can involve name lookups and a
script fetched from the local network. The code that can reach the network is the `tesela-update` crate, and
`scripts/check-pe.py` fails the build if the executables import any network module other than `winhttp.dll`.
**[TO CONFIRM: that no dependency makes network requests in the version that gets signed (apart from that check); if any
affects privacy, SignPath asks to link its policy.]**

## Reputation and contact

- Open source project with an **MIT or Apache-2.0** license (at the user's choice), with public history at
  [github.com/wandoth1/tesela](https://github.com/wandoth1/tesela) and public CI.
- Vulnerabilities: see [SECURITY.md](SECURITY.md).
- **[TO COMPLETE for the application: the project's contact address, the legal name or pseudonym under which signing is requested,
  and that the home page and the downloads page show this policy and the attribution.]**
- SignPath Foundation requirements that are **not yet met** and must be watched before applying: verifiable reputation
  (a project with enough use and age) and that the repository has no reverse-engineering work or potentially
  unwanted software (Tesela deletes the user's files only after an explicit confirmation; it is worth saying so in
  the application).
