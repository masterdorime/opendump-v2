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

`crates/opendump-core` — the classifier, ported deliberately rather than transliterated.

| Module | Contents |
| --- | --- |
| `types` | `Tier`, `Rule`, `RuleMatch`, `DirEntry` |
| `tokens` | `%LOCALAPPDATA%`-style expansion against an injectable map |
| `rules` | rule compilation, AND matcher semantics, `find_candidates` |

Everything is covered by tests that encode the same expectations as the TypeScript suite:

```bash
cargo test
# 10 passed
```

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

- **Scanner** — filesystem walk, and later the MFT reader that motivates Rust in the first place
- **Executor** — quarantine, restore, purge, journal
- **Tauri shell** — window, IPC, UI
- **AI layer** — Ollama provider, hardware gating
- **`find_unclassified`** — the AI's target set

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
