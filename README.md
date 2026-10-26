# opendump v2

> **Pre-alpha. This is not an app yet.** It is the deterministic core, rewritten in Rust.
> The working release is still [opendump v0.5.0](https://github.com/masterdorime/opendump/releases) —
> an Electron app that scans, classifies, quarantines and purges today.

This repository is the Rust rewrite of opendump's engine, heading for a Tauri shell. It exists
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
`crates/opendump-cli` — a read-only command-line front end.

| Module | Contents |
| --- | --- |
| `types` | `Tier`, `Rule`, `RuleMatch`, `DirEntry` |
| `tokens` | `%LOCALAPPDATA%`-style expansion against an injectable map |
| `rules` | rule compilation, AND matcher semantics, `find_candidates` |
| `scan` | recursive walk with recursive sizes and `package.json` detection |

```bash
cargo test          # 13 tests: 10 for rules, 3 for the scanner
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

**It is read-only.** There is no executor in this repository; nothing here removes or renames
a file.

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

## What is not here yet

- **Executor** — quarantine, restore, purge, journal
- **Tauri shell** — window, IPC, UI
- **AI layer** — Ollama provider, hardware gating
- **`find_unclassified`** — the AI's target set
- **The MFT scanner** — the current walk uses `read_dir`. Parsing `$MFT` directly is the next
  step and the real reason for Rust; `scan.rs` is written so it can be replaced without touching
  `rules.rs`.

Nothing in this repository can delete a file. That is deliberate: the executor lands only after
the scanner and its tests do.

## Build

```bash
cargo test                      # 10 tests
cargo build --release
```

Requires Rust (MSVC toolchain on Windows) and the Visual Studio Build Tools.

## Relationship to v1

v1 is not deprecated. It works, it has releases, and it is what people should use today. This
repository will earn the name "v2" when it can scan, classify, quarantine and purge — not before.

MIT licensed.
