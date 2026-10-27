# AGENTS.md — opendump-v2

The Rust rewrite of the opendump engine, plus a native window. Read this before changing
anything; it records the invariants and the traps that already cost time once.

**Companion repo (read its `AGENTS.md` for the full story):**
`C:\Users\TRISTAN\drive size audit` → https://github.com/masterdorime/opendump — the Electron
app that is still the shipped product (v0.5.0, 5 releases). This repo has **no release** and
should not be handed to a user as their daily driver yet: no treemap, no tour, no AI.

**Last updated:** 2026-10-27 · last code commit `90ea880` (docs commits follow) · working tree
clean · CI green.

---

## 1. State

| | |
| --- | --- |
| Version | 0.1.0, unreleased |
| Tests | **28** (`cargo test --workspace`) |
| Lines | **3,121** Rust |
| Binaries | `opendump.exe` 1.91 MB · `opendump-native.exe` 6.29 MB |
| Measured | full `C:\` scan **91.7–102.8 s**, **89 MB** idle RAM (one process), 107 MB after a scan |

Correctness anchor: totals match `robocopy /L` exactly on `Users` (223.1 GB / 999,946 files),
`Windows`, `ProgramData`, `Program Files`.

---

## 2. Layout

```
crates/opendump-core/   1,334 src + 467 test lines
  types.rs      Tier · Rule · RuleMatch · DirEntry
  tokens.rs     %LOCALAPPDATA%-style expansion, injectable map (tests rely on this)
  rules.rs      compile · AND matchers · find_candidates · parent_of/depth_of (pub(crate))
  scan.rs       recursive walk; one public fn scan(root) -> ScanReport
  executor.rs   quarantine · restore · purge · journal · inspect_staged
  format.rs     bytes() · thousands()  — shared by CLI and window, do not duplicate

crates/opendump-cli/    448 lines   opendump.exe
apps/native/            872 lines   opendump-native.exe (egui + glow)
```

`apps/native/src/main.rs` is one file on purpose:

```
consts    BG PANEL LINE TEXT MUTED DIM ACCENT + SAFE REVIEW KEEP LOCKED  (whole palette)
structs   ScanView · StagedView · ConfirmState · Message
impl App  lines 165–316   logic:  start_scan · quarantine_picked · restore_all · purge_ids · drain
impl      eframe::App     update()
impl App  lines 497–889   views:  summary · review · quarantine_panel · confirm_modal
helpers   pretty · tier_color · tier_label · install_fonts · install_style · stat · selftest · main
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
cargo run --release -- --selftest  # headless scan + classify + print, no window
```

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
   `{ root, dirs, total_bytes, total_files, skipped, duration_ms }`.
5. **The native app throws `report.dirs` away** after `find_candidates(..., 1024 * 1024)`.
   `ScanView` keeps only root/folders/files/bytes/duration/candidates. Anything tree-shaped
   (treemap) must retain the directories first.
6. **`parent_of` / `depth_of` are `pub(crate)`** in `rules.rs`. Group-by-parent needs them `pub`.
7. **Path checks, not string checks.** `%LOCALAPPDATA%`-style tokens are expanded through the
   injectable map in `tokens.rs` — tests inject a fake map, so never call `std::env` directly in
   that path.

---

## 7. What is next

**Treemap.** The signature view and the only obvious missing feature. Suggested order: make
`parent_of`/`depth_of` public → retain a pruned `dirs` subset in `ScanView` (drop under ~0.02 %
of the root, as v1 does) → group children by parent → squarify into `egui::Rect`s → paint with
`ui.painter().rect_filled()` → hover tooltip, click-to-zoom, breadcrumb. Visual reference:
v1's `src/renderer/src/components/Treemap.tsx` (d3 squarified).

Then, in order: live RAM/VRAM meters (v2 has no hardware probe at all; RAM is the AI gate),
guided tour (v1 `TourOverlay.tsx`), AI panel (v1 `src/shared/ai/*`), MFT scanner.

Beyond the UI, the highest-value work in either repo is the **inspection layer** — reading
folder contents instead of guessing from names. That is v1 work today.

---

## 8. Verifying

- Engine: `cargo run --release -- --selftest` — folders, files, size, duration, tier counts,
  no window. Rescan costs ~100 s; there is no cache yet.
- Window: egui has no debug port. Drive it with Win32 — `SetWindowPos` to a known origin
  (`0, 0, 1240, 820`), `SetForegroundWindow`, `SetCursorPos` + `mouse_event`, then
  `Graphics.CopyFromScreen`. With one item staged, the quarantine "Purge all" button is at
  ≈ (1180, 198).
- Do not delete the user's staged data while testing. Use `--journal` / `--quarantine` pointing
  at a temp directory, and clean up the demo entry afterwards.

## 9. Do not

- Add a webview front end. A Tauri shell was built and deleted: 9.98 MB exe, but 316 MB of
  WebView2 at runtime, and it duplicated the window that already existed.
- Claim this is ready for users. It has no treemap, no tour, no AI, no signed build, and has
  only ever run on one machine at one DPI.
- Round-trip source through PowerShell — `Set-Content -Encoding utf8` writes a BOM and
  `Get-Content`/`Set-Content` mangles non-ASCII. Use the editor tools or Node with `utf8`.
