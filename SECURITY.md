# Security policy

## How to report a vulnerability

**Do not publish the details of a vulnerability in a public issue.** There are two channels:

1. **GitHub private advisory (preferred):** the **Security** tab of the repository, **Report a vulnerability** button
   ([github.com/wandoth1/tesela/security/advisories/new](https://github.com/wandoth1/tesela/security/advisories/new)).
   Only the maintainers can see it. *If the button does not appear, private reporting is not enabled in the repository yet: use
   channel 2.*
2. **Fallback channel (available today):** open an [issue](https://github.com/wandoth1/tesela/issues/new) **WITHOUT technical
   details** (no steps, no screenshots, no data): just say that you want to report a security problem and ask for a private
   channel. A maintainer will answer in the issue with how to proceed, and the issue will be deleted or left without details.

Once the advisory is confirmed, the fix is prepared in private, a corrected version is published and then the advisory (crediting
whoever reported it, if they want).

When reporting, describe: Tesela version, what happens, how to reproduce it and what impact it has. **Do not include personal data**
(names of real folders or files, your user name, screenshots of your disk): to reproduce a bug a test folder or `--demo` mode,
which uses synthetic data, is enough.

## What counts as a vulnerability

Tesela **deletes files** and installs without administrator rights, so the cases of most interest are:

- Tesela deletes or moves something that was not in the confirmation (outside the approved set, or by following a link, junction
  or a folder swapped in the middle of the operation).
- A protected folder (Windows, Program Files, `AppData`, Tesela itself) can be cleaned.
- Privilege escalation, or code execution, through the MSI installer, the portable zip or the executables.
- A published package that does not match its `SHA256SUMS.txt` or its provenance attestation, or that carries data from the
  machine that built it.
- The application sends information off the computer (it must not: it has no telemetry or accounts).

## Elevation

Tesela **does not clean with elevated privileges** (scanning and simulation do work): if the process is elevated, real
cleaning and emptying the Recycle Bin are rejected before anything is touched. With User Account Control (UAC) turned off, or with the built-in
Administrator account, every process counts as elevated and Tesela will always refuse to clean: use a standard
user account (or with UAC on) and open it with a normal double-click, not "Run as administrator".

## Supported versions

Only the **latest published version** receives security fixes (today, the 0.x series).

## Timelines

The aim is to acknowledge receipt within a few days and give a first analysis within two weeks. This is a personal project with no service
guarantees; if a report goes unanswered, you can follow up on the same advisory or issue.
