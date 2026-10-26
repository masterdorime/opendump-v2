# opendump v2

> **Pre-alpha.** The engine is complete and there is now a minimal desktop window. The supported
> release is still [opendump v0.5.0](https://github.com/masterdorime/opendump/releases) — it has
> the full UI, local AI, and an installer.

This repository is the Rust rewrite of opendump's engine, with a native window. It exists
separately because it is a different stack with a different build, and because the Electron app
should stay the supported release until this one can do the same job.

## Why rewrite at all

Measured on the v1 build, not estimated:

| | opendump v0.5 (Electron) |
| --- | --- |
| Installer / portable | 116.6 MB / 105.7 MB |
| Installed directory | **366 MB** |
| Idle RAM (4 processes) | **377 MB** |
| Actual product code | 8.8 MB (`app.asar`, ~5,400 lines) |

A disk-cleanup tool that occupies 366 MB of disk and 377 MB of RAM is a fair criticism, and the
ratio — 8.8 MB of product inside 366 MB of runtime — is the real argument.

Rust also opens the door to the one thing JavaScript cannot fix: **the scan**. v1 takes 172–299 s
because it asks the filesystem about ~1.86 million files one at a time. Reading the NTFS MFT
directly is a plausibly sub-10-second operation, and that is a native-code problem.

## What is here

`crates/opendump-core` — the engine, ported deliberately rather than transliterated.
`crates/opendump-cli` — a command-line front end. `scan` is read-only; `quarantine`, `restore`
and `purge` require explicit confirmation flags.

| Module | Contents |
| --- | --- |
| `types` | `Tier`, `Rule`, `RuleMatch`, `DirEntry` |
| `tokens` | `%LOCALAPPDATA%`-style expansion against an injectable map |
| `rules` | rule compilation, AND matcher semantics, `find_candidates` |
| `scan` | recursive walk with recursive sizes and `package.json` detection |
| `executor` | quarantine / restore / purge, journal-first |
| `format` | byte and count formatting, shared by every front end |

```bash
cargo test          # 26 tests: 10 rules, 3 scanner, 11 executor, 2 formatting
cargo build --release
```

### Try it

```bash
./target/release/opendump scan "C:\Users\you\AppData\Local" --top 20
```

```
opendump 0.1.0 — rust core
root      C:\Users\you\AppData\Local
rules     rules/rules.json (80 loaded)

folders   87,008
files     551,683
size      87.2 GB
scan      21.96s  (3,963 folders/s)

candidates (>= 1.0 MB exclusive)
  SAFE_PURGE     4       8.7 GB
  REVIEW         7      29.8 GB
  KEEP          41      31.5 GB
  LOCKED         0          0 B

top 10 by reclaimed bytes
  KEEP           14.6 GB  ...\CapCut  [holds 2.4 GB protected]
  REVIEW          5.5 GB  ...\CapCut\User Data\Cache
  SAFE_PURGE      6.2 GB  ...\npm-cache
```

**It is read-only.** `scan` never removes, renames or writes anything.

### Cleaning (two deliberate steps)

```bash
opendump quarantine "$LOCALAPPDATA\Temp\some-cache"        # prints the plan, does nothing
opendump quarantine "$LOCALAPPDATA\Temp\some-cache" --yes  # moves it into quarantine
opendump list                                              # what is staged
opendump restore --all                                     # undo
opendump purge --all --yes                                 # reclaim the space
```

The guards, in order:

- **`quarantine` refuses any folder the rules do not classify as removable.** It re-classifies
  the path you gave it and prints which rule decided. A folder that only matches the broad
  `KEEP` catch-all is refused:

  ```
  refused  C:\Users\you\some-folder: classified KEEP by keep-profile
  ```

- **`--allow-review`** is required to accept `REVIEW` folders; nothing accepts `KEEP` or
  `LOCKED`, and the executor refuses those tiers again independently.
- **Nothing happens without `--yes`.** Both `quarantine` and `purge` print their plan and exit 1.
- **Purge is irreversible.** It prints the byte count and refuses without `--yes`.
- Cross-volume moves are refused: the destination must share the source's volume, because a move
  must be a rename.

### Measured against the Electron build

| | v0.5 (Electron/JS) | v2 (Rust) |
| --- | --- | --- |
| Full `C:\` scan | 172 s idle / 299 s loaded | **102.8 s** |
| Binary / install | 366 MB installed | **1.75 MB** |
| Folders/sec | ~2,200 | ~3,530 |

Same classifications: `CapCut\Cache` → REVIEW, `CapCut\Projects` → KEEP, `npm-cache` →
SAFE_PURGE, and `CapCut` flagged as *holds 2.4 GB protected*.

Verified against `robocopy /L` on four subtrees — `Users`, `Windows`, `ProgramData`,
`Program Files` — all matched to within noise.

> **Note on totals:** the full-drive sum is larger than the physical used space because hard
> links (WinSxS ↔ WindowsApps) are counted per path. `robocopy` behaves identically. v2 also
> counts `pagefile.sys` and `hiberfil.sys`, which v0.5 silently skipped — about 29 GB it was
> under-reporting.

`rules/rules.json` is copied verbatim from opendump v0.5 and must stay in sync until the two
repositories merge. The rule data is the product; only the engine changed.

### The invariants this crate is built around

1. **Matchers on one rule are ANDed** — `{ prefix, name }` requires both.
2. **A candidate reports exclusive bytes** — its own size minus nested candidate groups — so
   totals never double-count.
3. **Protection is monotonic downward** — a `KEEP` or `LOCKED` folder protects everything beneath
   it, and any parent holding such data is reported `blocked` rather than removable.
4. **`display` rules colour a map, protect nothing, and are never candidates.**

These are the same four guarantees the TypeScript engine enforces. Porting them was the point;
the language is not what makes opendump safe, the design is.

## Two front ends, one engine

| | v0.5 (Electron) | v2 native |
| --- | --- | --- |
| Rendered with | Chromium | **egui → GPU** |
| Installer | 116.6 MB | — (raw exe 6.17 MB) |
| Idle RAM | 377 MB | **89 MB, 1 process** |
| RAM after a scan | — | 107 MB |
| Full `C:\` scan | 172–299 s | **91.7 s** |

`apps/native` draws the same four regions — bar, summary, review list, quarantine — straight
to the GPU. There is no browser runtime, which is the whole difference: the webview alone costs
~316 MB before the app draws anything.

```bash
cd apps/native
cargo run --release                 # the window
cargo run --release -- --scan       # scan immediately on open
cargo run --release -- --selftest   # headless: scan, classify, print, exit
```

Purge is armed rather than immediate: the button reads "Purge all", then "Confirm: delete 12.4
GB?" on the second click. Deleting staged data is the one action with no undo.

What it is not: egui's stock widgets do not match a hand-styled CSS interface pixel for pixel,
and the treemap, guided tour and filename-by-filename confirmation are still only in v0.5. The
look is close — same palette, same rows, same tier colours — but it is a reimplementation.

## What is not here yet

- **v0.5's UI** — treemap, spotlight tour, purge confirmation listing filenames, live memory meters
- **AI layer** — Ollama provider, hardware gating
- **`find_unclassified`** — the AI's target set
- **The MFT scanner** — the current walk uses `read_dir`. Parsing `$MFT` directly is the next
  step and the real reason for Rust; `scan.rs` is written so it can be replaced without touching
  `rules.rs`.

The engine is complete enough to scan, classify, quarantine, restore and purge from either a
terminal or a window.

## Build

```bash
cargo test                      # 26 tests (workspace)
cargo build --release
```

Requires Rust (MSVC toolchain on Windows) and the Visual Studio Build Tools.

## Relationship to v1

v1 is not deprecated. It works, it has releases, and it is what people should use today. This
repository will earn the name "v2" when it can scan, classify, quarantine and purge — not before.

MIT licensed.
