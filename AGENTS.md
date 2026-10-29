# AGENTS.md — opendump-v2

The Rust rewrite of the opendump engine, plus a native window. Read this before changing
anything; it records the invariants and the traps that already cost time once.

**Companion repo (read its `AGENTS.md` for the full story):**
`C:\Users\TRISTAN\drive size audit` → https://github.com/masterdorime/opendump — the Electron
app that is still the shipped product (v0.5.0, 5 releases). This repo shipped **v0.1.0** on
2026-10-29 as an unsigned windows-x64 zip, and still should not be handed to a user as their daily
driver: there is no guided tour and no signed build.

**Last updated:** 2026-10-29 · last code commit `1bfd9ca` (docs commits follow) · working tree
clean · CI green.

> **Two standing rules, added 2026-10-29.**
>
> 1. **Map before you read.** `graphify-out/` (in the v1 repo root) holds a knowledge graph of
>    that repo; ask it where things are instead of opening files to find out. Rebuild with
>    `/graphify .` after structural change. For this repo, run `/graphify .` here once and the
>    same applies.
> 2. **Document every change in the same commit.** Counts, the file map, the roadmap, and any
>    decision a change contradicts. Unfinished work goes in the commit message, not left silent.

---

## 1. State

| | |
| --- | --- |
| Version | 0.1.0, **released 2026-10-29** (`v0.1.0`, unsigned windows-x64 zip) |
| Tests | **73** (`cargo test --workspace`) |
| Lines | **7,865** Rust |
| Binaries | `opendump.exe` 1.91 MB · `opendump-native.exe` 6.93 MB (7.25 MB with the mascot placeholder) |
| Measured | full `C:\` scan **4.9 s** via the MFT, **75 s** via the walk; **~95 MB** idle, **~130 MB** with a full map drawn |

The window has five views (Map / Review / Quarantine / AI / Rules), a nested treemap with
Status and Size colouring, an inspector with content inspection, light and dark themes on a
monochrome chrome with a red accent, a purge confirmation card, and press feedback on every
filled button. The scan reads the NTFS MFT when elevated and falls back to the walk.

Correctness anchor: totals match `robocopy /L` exactly on `Users` (223.1 GB / 999,946 files),
`Windows`, `ProgramData`, `Program Files`.

---

## 2. Layout

```
crates/opendump-core/   3,702 src + 568 test lines
  types.rs      Tier · Rule · RuleMatch · DirEntry
  tokens.rs     %LOCALAPPDATA%-style expansion, injectable map (tests rely on this)
  rules.rs      compile · AND matchers · find_candidates · parent_of/depth_of (public)
  scan.rs       recursive walk + scan_any (MFT first, walk as fallback)
  mft.rs        NTFS Master File Table reader: boot sector, fixups, run lists, tree build
  executor.rs   quarantine · restore · purge · journal · inspect_staged
  layout.rs     squarified treemap over plain Box4 rects — no UI dependency
  hardware.rs   GlobalMemoryStatusEx for RAM, nvidia-smi for free VRAM
  inspect.rs    folder contents: extension mix, largest files, hashed-vs-human names
  ai.rs         localhost Ollama client, prompt, schema, parse_verdicts (REVIEW ceiling)
  cache.rs      versioned binary scan cache, front-coded paths, rules-hash invalidated
  format.rs     bytes() · thousands()  — shared by CLI and window, do not duplicate

crates/opendump-cli/    448 lines          opendump.exe
apps/native/            3,147 lines        opendump-native.exe (egui + glow)
```

`apps/native/src/main.rs` is one file on purpose. Grep the function name; line numbers drift
every session and were removed here on purpose:

```
consts    BG PANEL LINE TEXT MUTED DIM ACCENT + SAFE REVIEW KEEP LOCKED + ON_ACCENT
structs   NodeInfo · Pane(Map/Review/Quarantine/Ai/Rules) · Visual · MapCache · ScanView ·
          StagedView · ConfirmState
logic     start_scan · quarantine_picked · restore_all · purge_ids · drain · ensure_visuals
views     summary · review · quarantine_panel · tabs · map · inspector · confirm_modal
helpers   build_visuals (recursive squarify) · tile_fill (depth lift) · space_meter · metric ·
          spawn_probe · pretty · last_component · clamp_label · on_fill · legend_item · stat
```

Work runs on `std::thread`; results come back as `Message` over an `mpsc` channel and are
drained at the top of `update()`. No async runtime, no retained scan tree.

---

## 3. Commands

```bash
cargo test --workspace

# CLI
cargo run --release -- scan "C:\" --top 20
cargo run --release -- quarantine <path> --yes
cargo run --release -- list
cargo run --release -- restore --all
cargo run --release -- purge --all --yes

# window
cd apps/native
cargo run --release              # opens empty
cargo run --release -- --scan    # scanning on open
cargo run --release -- --selftest  # headless scan + classify + print
cargo run --release -- --probe     # RAM/VRAM only, no scan (fast), no window

# mascot placeholder (temporary; see mascotdesign.md in this repo)
cargo run --release -- --mascot 96               # portrait height in the guide card
cargo run --release -- --pane quarantine         # open any pane directly
cargo run --release -- --mascot-image            # draw the baked art instead of the slot
cargo run --release -- --zoom "C:\...\npm-cache" # open the inspector on one folder
```

Navigation is a **left sidebar** (Map / Review / Quarantine, then Tools: Local AI, Rules); there is
no tab row. The right rail is always present: it inspects the drive or the zoomed folder and pins
**Sumi's guide card** at its bottom. She may only restate the matched rule's tier, size and reason
plus one action line — same ceiling as the AI, she never invents a verdict. The artwork is a
placeholder cut from a reference image and is **not** shippable — `mascotdesign.md` holds the spec.

User data: `%APPDATA%\opendump-native\` — `journal.jsonl`, `quarantine\<batch>\…`. Deliberately
separate from v1's `%APPDATA%\opendump`.

---

## 4. Invariants — do not weaken these

1. **Nothing deletes without an explicit, acknowledged confirmation.** `purge` is the only
   function that frees space, and the window must show the folder paths, the file count and the
   largest filenames before the confirm button enables.
2. **One module holds write power** (`executor.rs`). It refuses paths it did not classify as
   removable, refuses anything not inside a known quarantine batch, and journals intent before
   touching disk.
3. **Quarantine before purge.** A move must be a rename, so quarantine lives on the same volume
   as its source.
4. **Sizes are exclusive**: a candidate reports its own bytes minus nested candidates, so
   selecting parent and child never promises the same bytes twice.
5. **Protection is monotonic downward**: a `KEEP`/`LOCKED` folder protects its whole subtree; a
   parent containing one is `blocked` and never removable as a whole.
6. **Rule matchers are ANDed, never ORed.** ORing let installed apps' `node_modules` be offered
   for deletion. `display: true` means colour only and protects nothing.

---

## 5. Rules — the thing that surprises people

`rules/rules.json` (80 rules, ordered, first match wins) is duplicated from the v1 repo and the
two copies are byte-identical (SHA-256 `BFAC0CE9…`). Change both or neither.

**The native app `include_str!`s it at compile time** (line 23 of `main.rs`), so an edit needs a
rebuild before the window sees it. The CLI reads it from disk (cwd, beside the exe, or
`--rules <path>`) and picks edits up immediately.

---

## 6. Traps already hit — don't rediscover them

1. **Forward-slash paths bypassed the "inside quarantine" check.** `normalize`, `parent_of` and
   `depth_of` all normalise separators now. Keep it that way.
2. **Display rules were emitted as candidates.** The emit loop skips `display` rules; a
   catch-all like `keep-windows` must colour the map, not become a deletable row.
3. **An egui `Area`'s painter is clipped to the area's own content rect.** Painting the modal dim
   overlay through `ui.painter()` inside a zero-size area draws nothing. Use
   `ctx.layer_painter(LayerId::new(Order::Middle, …))`, and block clicks with a separate
   full-screen area.
4. **`scan()` returns a flat `Vec<DirEntry>`** (`path`, recursive `size`, `files`,
   `has_package_json`) — no parent links, no children. `ScanReport` is
   `{ root, dirs, total_bytes, total_files, skipped, duration_ms }`. The treemap builds its own
   tree from this in the scan thread; do not expect one from the scanner.
5. **The node graph is pruned on purpose.** `nodes`/`levels` keep directories above
   `total_bytes / 200_000`, clamped to 2 MB. Parents are always at least as large as their
   children, so nothing reachable is lost — but a folder whose contents are all small will show
   only a "files in this folder" tile.
6. **`depth_of` is a separator count, not a level number.** `C:\` and `C:\Users` both report 1.
   Only sort order relies on it; do not use it to compare nesting.
7. **`parent_of` and `depth_of` are public** because the map groups by parent. `parent_of`
   normalises both separators and returns `None` at a drive root.
8. **Path checks, not string checks.** `%LOCALAPPDATA%`-style tokens are expanded through the
   injectable map in `tokens.rs` — tests inject a fake map, so never call `std::env` directly in
   that path.
9. **A nested `ui.vertical` inside a right-to-left layout inherits almost no width** and wraps
   its text to one character per line — and the parent bar grows to fit. That is why the memory
   meters are single labels. Same family as trap 3: an egui child does not get the width you
   assume. Screenshot the header after touching it.

---

## 7. What is next

**Guided tour.** v1's `TourOverlay.tsx` is the reference: steps with a target rect, heading and
copy, everything else dimmed. In egui the dim is a `layer_painter` fill plus a hole — and note
trap 3 above, an `Area`'s painter is clipped to its own content.

Then: cloud AI provider (wired in v1, never tested live — test before porting), Exclude from Scan
(needs the scanner to take a list and the cache to invalidate on it), multi-drive, elevated tier.

**Release recipe** (v0.1.0 shipped this way, 2026-10-29):

```bash
cargo build --release                 # crates/opendump-cli + core
cd apps/native && cargo build --release   # the window (own workspace!)
# stage: opendump.exe, opendump-native.exe, rules/rules.json, README.md, LICENSE
# zip as opendump-<version>-windows-x64.zip, tag, gh release create
```

The zip must contain `rules/rules.json` beside the exes — the CLI resolves it there, the window
compiles the same file in with `include_str!`. A zip without it gives a rule-less CLI.

Already shipped and not to be rebuilt: treemap (`171017e`), meters (`502734b`), cache
(`498bb04`), local AI (`80a484a`), inspection layer and rules editor (`d026648`), themes
(`a7f0572`), MFT scanner (`9bb29e4` → `731e7b2`), purge dialog and press feedback
(`f945c5c`, `1bfd9ca`).
---

## 8. Verifying

- Engine: `cargo run --release -- --selftest` — folders, files, size, duration, tier counts,
  no window. A cold rescan of `C:\` costs ~90–100 s; the window reuses `scan-cache.bin` on open
  (rules-hash invalidated), but there is no CLI re-classify switch — editing `rules.json` and
  relaunching is the current loop.
- Window: egui has no debug port. Drive it with Win32 — `SetWindowPos` to a known origin
  (`0, 0, 1240, 820`), `SetForegroundWindow`, `SetCursorPos` + `mouse_event`, then
  `Graphics.CopyFromScreen`. With one item staged, the quarantine "Purge all" button is at
  ≈ (1180, 198).
- Do not delete the user's staged data while testing. Use `--journal` / `--quarantine` pointing
  at a temp directory, and clean up the demo entry afterwards.

## 9. Do not

- Add a webview front end. A Tauri shell was built and deleted: 9.98 MB exe, but 316 MB of
  WebView2 at runtime, and it duplicated the window that already existed.
- Claim this is ready for users. It has no guided tour and no signed build, and
  has only ever run on one machine at one DPI.
- Round-trip source through PowerShell — `Set-Content -Encoding utf8` writes a BOM and
  `Get-Content`/`Set-Content` mangles non-ASCII. Use the editor tools or Node with `utf8`.
