# Product roadmap

Date: 2026-10-16. Replaces the phase table in `PLAN.md` §4 for everything that comes after v0.1. The technical design
remains in `PLAN.md`.

Origin: a differentiation study with sources and two independent external reviews of the plan. The claims
about the competition must be checked again before they are published.

## 1. Thesis

Seeing what takes up the disk is already solved, and well: WizTree, WinDirStat, TreeSize, and Windows' Storage Sense and
recommendations. Tesela does not compete on scan speed or on chart types.

Tesela competes on **helping to decide how to recover space**: what each thing is, what happens if you remove it, what the
documented way to do it is and what actually happened afterwards. Someone can use another tool to find the
space and Tesela to decide what to do with it.

**Positioning:**
> Tesela explains what it recognizes, points to documented methods and only offers cleanup actions under
> verified conditions.

Short tagline: *"Understand your disk before you delete."*

Never "risk-free", "fearless" or "proven safe". A file's identity proves which object was opened, not
that deleting it is harmless.

## 2. Initial segment (hypothesis to validate)

- **Who adopts it:** the person who regularly helps relatives or acquaintances with space problems on home
  Windows machines, when Settings and Explorer do not make it clear. They compare, install, repeat and recommend.
- **Who benefits:** whoever has a full C: drive, often on a laptop with a small SSD.
- **Out for now:**
  - managed corporate machines (policies, permissions and support);
  - the developer segment, which is secondary and is left for after validation.

## 3. Product rules

1. **Recognizing is not authorizing.** A path, an anchor file, `CACHEDIR.TAG` or a folder name only produce an
   explanation. They never preselect anything nor lift a protection.
2. **Tesela does not run elevated commands or third-party tools for the user.** It explains, opens the exact Settings
   page (`ms-settings:`) or shows and copies the official command with its effects and its administrator warning.
3. **Three visible states:** *unknown*, *known but not recommended* and *candidate with conditions*. The unknown
   never gets an invented explanation.
4. **Two distinct outcomes:**
   - "I avoided harm";
   - "I solved the lack of space".

   They are counted separately. A product for a full C: drive needs mostly the second.
5. **Ways out that free space without Tesela deleting anything delicate:**
   - free the local copy of OneDrive (the file stays in the cloud);
   - move data to another drive;
   - go to Installed apps;
   - use Windows' official methods.
6. **No telemetry**, not even "anonymous".
7. **Never clean elevated.** An elevated read, if it comes, goes in a separate helper, optional and read-only, with
   minimal capabilities.

## 4. Knowledge catalog

Explanations and recommendations are **data** (embedded TOML) with a validator in CI. Each entry carries:

- a primary source (vendor or Microsoft Learn), a review date and an expiry **proportional to its rate of change**;
- preconditions, effects, action (delete, Recycle Bin, official tool, open Settings or do not touch),
  recovery and **conditions to abstain**;
- positive and negative tests.

In addition:

- When an entry expires it stops recommending any action, **also the command to copy**. The explanation can
  still be shown with a warning.
- An entry with a destructive action needs "verified" confidence.
- The log of each cleanup stores the **rule version** and its preconditions, in addition to the file's
  identity.
- A catalog contribution is reviewed as if it were code: it checks the interpretation, the scope and the
  applicability, not just that it has a source.
- The value is not the text, which can be copied. It is the whole:

  > recognize the context → recommend what is appropriate → abstain when data is missing → check what happened

  backed by a corpus of cases and regressions.

## 5. Validation phase (3 months)

Before expanding features we must show that Tesela solves space problems better than what already exists.

### Month 1: safety perimeter and public evidence

- **Publish v0.1.2.** It carries the audit fixes and honest texts.
- **Public safety battery** in a disposable virtual machine, published with each release:
  - sentinel files in the protected zones;
  - races, junctions and substitutions;
  - almost full disk: no space to install or to write the log;
  - log write failures, changing contents, and recovery with conflicts.
- **Behavior with an almost full disk:** diagnosis-and-guidance-only mode. It never skips the log.
- **Signing:** apply to SignPath and publish on winget, in parallel and without depending on either.

### Month 2: a few complete walkthroughs built from real cases

- Gather the following cases from about 10 people who help others, with consent and with minimal personal
  data.
- Build **6–8 complete walkthroughs**, chosen by the observed frequency and because they really free space. There is
  no need for 15 cards.
- The "What is this?" panel, the history and the minimal exclusions exist to serve those walkthroughs.
- **Measure before deciding:** how many "System and reserved" cases cannot be solved without seeing what the user
  cannot read. If there are many, the elevated read-only helper stops being something for later and moves into the core.
  Reusing the current enumerator is evaluated first; the MFT only if needed.

### Month 3: a pilot without the author's help

- About 30 cases, between reproducible scenarios and real cases. They must not be only the cases the rules were
  designed with.
- **Comparison baseline:**
  - for the helper, WinDirStat + Settings;
  - for the general user, Explorer + Settings.
- The complete walkthrough is measured: install, understand, act and verify.

### Decision metric

> **Percentage of space problems solved correctly, without the author's intervention and without harmful actions.**

- **Target:** set in advance. Either recover the space a task needed, or complete a specific
  diagnosis. "Do not touch this" does not count as space freed.
- **Indicative threshold:** at least 70% of cases solved and 20 points above the usual alternative, with the
  safety battery passed.
- **Complementary signal:** the helper chooses Tesela again in another case without being asked.
- **If there is no advantage:** no more features are added. The scope is cut, the segment is revised or the
  catalog and its tests are turned into a component for other tools.
- **If there is a safety incident:** the affected feature is stopped. A good average of usefulness does not make up for it.

Thirty cases are a pilot to guide the project, not a statistical certification.

## 6. After validation (only if the metric comes out well)

- **v0.2, "Understand before deleting":**
  - the validated walkthroughs in the "What is this?" panel, with their official route;
  - history and Undo with a contract;
  - large and old files with context;
  - minimal exclusions;
  - a simple report to share with the helper, which hides sensitive paths and names;
  - checked basic accessibility: keyboard, focus, scaling and reading of the decisions.
- **Undo contract:**
  - it only undoes what Tesela did and that is still in the Recycle Bin;
  - it verifies by identity and never overwrites;
  - it gives the result item by item: restored, no longer there, conflict or changed;
  - what was done by Windows, OneDrive or other tools is out of scope and it says so.
- **Seeing the inaccessible:** an elevated read-only helper that names what the user cannot read, without offering to
  delete it. It comes sooner or later depending on what is measured in month 2.
- **Technical segment** (later): development caches through their official command, with its exact scope (for
  example, `docker builder prune` instead of `docker system prune` if only the build cache is wanted; `uv cache
  clean` or `prune` as appropriate) and project folders with abstention conditions (uncommitted changes,
  links, no lockfile).
- **Postponed until there is evidence:** duplicates, incremental rescan (USN), published ARM64 and full export.

## 7. Anti-goals

- Competing on scan speed or chart types.
- Registry cleaner, "optimize", PC health score, uninstaller.
- Automatic or scheduled cleaning.
- Running DISM, powercfg, docker, uv or other tools on the user's behalf.
- AI-generated explanations without a source, or rules imported from third-party catalogs.
- Promising regeneration figures or "risk-free".
- Recommending something because of its size or its extension (for example, a `.db-wal` may contain committed data of a
  database).

## 8. Complementary metrics

- **Safety battery:**
  - 0 sentinels touched and 0 changes outside what was approved (a publication requirement, not an estimate of real
    safety);
  - plus that the allowed operations **work**. A tool that always abstains also passes the negative
    tests.
- **Catalog:**
  - active operational recommendations within their validity;
  - useful coverage of the space that really gets used;
  - time spent reviewing it. "0 expired" is not measured, since it invites renewing dates superficially.
- **User studies of 5 at a time:** they serve to find comprehension problems, not to claim that dangerous mistakes
  are rare. The critical questions (for example, "does this also delete the cloud copy?") must all be
  answered correctly, not just on average.
- **Downloads and issues:** they are auxiliary signals, not a denominator of harm or of usefulness.

## 9. Risks and open tasks

- **Antivirus:** a heuristic antivirus may distrust the behavior of a cleaner (mass deletion, use of the
  Recycle Bin through COM). It already happened with the **test** binary when it was run in a loop. Before announcing the cleaner,
  `tesela.exe` must be tested with the most common antivirus products (Microsoft Defender and two or three others) and, if any flags it,
  the false positive must be reported to the vendor. Code signing helps reputation, but does not solve it by itself.
- **Microsoft Store channel (experiment, not decided):** it is the only way to avoid the SmartScreen warning entirely,
  because Microsoft signs the package. It requires MSIX. A `runFullTrust` desktop app can read the whole disk, but
  Windows **virtualizes writes to AppData**, which is exactly where the caches and temporary files being cleaned live.
  Before deciding we must test whether, with the restricted `unvirtualizedResources` capability, cleaning
  AppData works exactly the same as with the MSI, and whether the Store accepts it for a cleaner. Until then the MSI
  remains the main channel.
- **Signing:** SignPath is not guaranteed (it asks for verifiable reputation and a privacy policy). The real availability
  and cost of the alternatives must be confirmed before counting on them.
- **Liability:** the license does not give automatic immunity. Before promoting the cleaner heavily, a scoped legal
  review of the promises, the license and incident handling is advisable.
- **Catalog maintenance:** sources change. Expiry is a review mechanism, not proof that
  something is still true.
- **The "low space" threshold:** the percentage is combined with the absolute space and the context. The "I need
  space" walkthrough is always accessible and never turns into a "health" alarm.
