# AGENTS.md — opendump-v2

**Read this first. It is written for an agent who knows nothing about this project.**

`opendump` is a Windows disk-audit tool: it scans a drive, sizes every folder, classifies each one by
how safe it is to remove, and reclaims space through a **quarantine → purge** flow that journals its
intent before touching disk. This repository is the **Rust rewrite** of it, and it is the one being
actively developed.

The Electron version in `C:\Users\TRISTAN\drive size audit` (repo `masterdorime/opendump`) is still the
older, wider-audience product. **You are not working on it unless asked.** Everything below is this
repo.

**Last updated:** 2026-11-06 · release `v0.2.0` (2026-11-05) · `main` is 4 commits ahead of that tag
· both repos clean · CI green.

---

## 1. What exists right now

| | |
| --- | --- |
| Version | **0.2.0**, released 2026-11-05 as `v0.2.0` (unsigned zip **+** per-user installer) |
| Tests | **78** (`cargo test --workspace`) |
| Lines | **9,588** Rust across three crates |
| Binaries | `opendump.exe` **1.9 MB** (CLI) · `opendump-native.exe` **10.4 MB** (window) |
| Release assets | zip **5.09 MB** · installer **5.56 MB** |
| Measured scan | **4.9 s** reading the NTFS MFT (elevated) · **75 s** walking; **164 s** observed on a loaded machine 2026-11-04 |
| Measured RAM | **120 MB** idle (2026-11-06) · 118 MB with a folder inspector open |
| Platform | Windows 10/11 x64. Only ever run on Windows 11, one GPU, one display scaling |

Two binaries, two front ends, one engine:

- **`opendump.exe`** — CLI: `scan · quarantine · list · restore · purge`. This is what automated
  testing uses.
- **`opendump-native.exe`** — the egui window with five panes (Map / Review / Quarantine / AI / Rules),
  a nested treemap, a persistent right rail, and an eight-step guided tour.

Correctness anchor: totals match `robocopy /L` exactly on `Users` (223.1 GB / 999,946 files),
`Windows`, `ProgramData`, `Program Files`.

**The one invariant everything serves:** nothing deletes without an explicit, acknowledged
confirmation; one module holds write power; it refuses what it did not classify as removable; and it
journals intent before touching disk.

---

## 2. Read in this order

1. **§3 layout** — where a feature lives.
2. **§5 invariants** — the six rules that must not be weakened to make a feature easier.
3. **§8 traps** — twelve things that already cost a session each. Skim before touching the UI.
4. **§9 done / §10 not done** — do not rebuild what exists, and do not claim what is unproven.
5. **§11 next** — the ordered queue, with the first action for each.

If you only have ten minutes: read §1, §5, §9 and §10.

---

## 3. Layout

```
crates/opendump-core/          the engine — no UI, no Electron, no async runtime
  types.rs      Tier · Rule · RuleMatch · DirEntry
  tokens.rs     %LOCALAPPDATA%-style expansion, injectable map (tests inject a fake)
  rules.rs      compile · AND matchers · find_candidates · find_unclassified · parent_of/depth_of
  scan.rs       recursive walk, plus scan_any_with(root, exclusions, …): MFT first, walk fallback
  mft.rs        NTFS Master File Table reader: boot sector, fixups, run lists, tree build
  executor.rs   quarantine · restore · purge · journal · inspect_staged · stage_root_for
  layout.rs     squarified treemap over plain Box4 rects — no UI dependency, unit tested
  hardware.rs   GlobalMemoryStatusEx RAM · nvidia-smi free VRAM · GetDiskFreeSpaceEx
  inspect.rs    folder contents: extension mix, largest files, hashed-vs-human names
  ai.rs         localhost Ollama client, prompt, schema, parse_verdicts (REVIEW ceiling)
  cloud.rs      opt-in OpenAI-compatible client: payload preview, 45 s timeout, same parse_verdicts
  cache.rs      versioned binary scan cache, front-coded paths, rules_hash + exclusions_hash
  format.rs     bytes() · thousands() — shared by every front end, do not duplicate

crates/opendump-cli/          448 lines — opendump.exe
apps/native/                  ~4,900 lines — opendump-native.exe (egui + glow)
installer/                    opendump.iss + build.ps1 (Inno Setup 6)
docs/                         README screenshots: logo, map, review, guide, tour
mascotdesign.md               the mascot spec (art is placeholder)
```

`apps/native/src/main.rs` is **one file on purpose**. Grep the function name; line numbers drift.

```
consts    Palette (dark/light) + INK; pal() reads a static flag
structs   App · NodeInfo · Pane(Map/Review/Quarantine/Ai/Rules) · Visual · MapCache · ScanView ·
          StagedView · ConfirmState · Message · Settings · CloudSettings · TourStep · MascotMood
logic     start_scan · quarantine_picked · restore_all · purge_ids · drain · ensure_visuals ·
          run_ai · end_tour
views     sidebar · summary · review · quarantine_panel · map · inspector · ai_pane · rules_pane ·
          cloud_section · confirm_modal · empty_state · guide card (inside inspector)
tour      tour_overlay · tour_portrait · TOUR_STEPS · mascot_frame · paint_mascot
helpers   build_visuals · tile_fill · size_fill · chip · chip_at · nav_item · summary_card ·
          space_meter · draw_inspection · press/on_fill · clamp_label · ease_rect · bands_around
```

Work runs on `std::thread`; results return as `Message` over an `mpsc` channel drained at the top of
`update()`. No async runtime, no task graph.

**Rules load differently in the two front ends — this bites.** The CLI reads `rules/rules.json` from
disk (cwd, beside the exe, or `--rules`). The window `include_str!`s it at **compile time**, so an
edit needs a rebuild before the window sees it. The v1 and v2 copies are byte-identical and must stay
that way.

---

## 4. Build, run, verify

```bash
cargo test --workspace                     # 78 tests
cargo build --release                      # CLI + core

cd apps/native && cargo build --release    # the window (it is its OWN workspace)
```

Window flags — all of these exist for a reason and are used for verification:

```
--scan                 scan on open
--fresh                ignore the scan cache, start with nothing (tour begins at Press Scan)
--pane <name>          map | review | quarantine | ai | rules
--zoom <path>          open the inspector on one folder
--mood <name>          force a mascot frame: idle blink scan review sleep wave point done
--tour                 replay the tour from the start
--tour-step <n>        open the tour at step n
--tour-demo            advance a step every 2 s — recording only, ignores the real actions
--no-tour              never show the tour (use in scripted runs)
--mascot <px>          guide-card portrait height
--selftest             headless: scan, classify, print, exit
--probe                RAM/VRAM only, no scan, no window
```

Verification on this machine:

- **Engine:** `--selftest` prints folders/files/size/duration/tier counts. A cold `C:\` walk is
  ~90–165 s depending on load; the MFT path needs elevation and takes ~5 s.
- **Window:** egui has no debug port. Drive it with Win32 — `SetWindowPos` to `(0,0)` with outer
  `1264×851` (client `1240×820`, client origin `(8,31)`), `SetForegroundWindow`, then either
  `mouse_event` or `PostMessage(WM_LBUTTONDOWN/UP)` with **client** coordinates, and capture with
  `Graphics.CopyFromScreen`. PostMessage is more reliable than synthetic mouse moves here, and the
  app must actually be foreground or the click is swallowed.
- **Never test against the user's staged data.** Point `--journal` / `--quarantine` at a temp dir.
  A tour run *will* quarantine whatever you tick; it also restores it, but check the journal after.

---

## 5. Invariants — do not weaken these

1. **Nothing deletes without an explicit, acknowledged confirmation.** `purge` is the only function
   that frees space; the window shows paths, file count and the largest filenames before the button
   enables.
2. **One module holds write power** (`executor.rs`). It refuses what it did not classify as
   removable, refuses anything outside a known quarantine batch, and journals before touching disk.
3. **Quarantine before purge.** A move must be a rename, so staging lives on the source's volume:
   `stage_root_for` uses the configured root when volumes match, otherwise
   `<volume>:\DriveSizeAuditQuarantine`.
4. **Sizes are exclusive** — a candidate reports its own bytes minus nested candidates.
5. **Protection is monotonic downward** — a `KEEP`/`LOCKED` folder protects its subtree; a parent
   holding one is `blocked`, never removable whole.
6. **Rule matchers are ANDed, never ORed.** `display: true` means colour only and protects nothing.
7. **The guide card and the AI can only restate or suggest.** Verdicts are capped at `REVIEW` by
   `parse_verdicts`; only a deterministic rule may say `SAFE_PURGE`.

---

## 6. Rules engine

`rules/rules.json` — 80 rules, **one ordered list, first match wins**. Fields: `id`, `match`
(`prefix` / `name` / `regex`, all must pass), `requiresSibling` (checks the **parent** folder),
`tier`, `reason` (user-facing prose), `display`.

User overrides live in `rules/overrides.json` (gitignored) and are **prepended**, so they win.

A rule change is a data change: add the rule above the general one it must outrank, then add a test
that asserts both the folder it should match **and** the neighbour it must not. Nearly every rule bug
here has been an over-match, not an under-match.

---

## 7. Interfaces and data

- **CLI:** `scan | quarantine | list | restore | purge`, plus `--rules`, `--top`, `--min-mb`,
  `--journal`, `--quarantine`.
- **App data:** `%APPDATA%\opendump-native\`
  - `journal.jsonl` — append-only audit trail (`begin`, `quarantine`, `restore`, `purge`)
  - `quarantine\<batch>\C\Users\…` — staged folders, mirrored by path
  - `scan-cache.bin` — versioned cache, invalidated by `rules_hash` + `exclusions_hash`
  - `settings.json` — `{light, mascot, tour_seen, tour_step}` (every key is written on every save)
  - `exclusions.json` — `{paths: [...]}`, folders the scan skips entirely
  - `cloud.json` — `{enabled, endpoint, model}`. **The API key is never written**; it lives in memory
- **The MFT probe dump** goes to `%TEMP%\opendump-mft-records.bin`; a legacy copy in the profile is
  deleted on launch.

---

## 8. Traps already hit — twelve sessions' worth

1. **Forward-slash paths bypassed the "inside quarantine" check** → `normalize`/`parent_of`/`depth_of`
   all normalise separators.
2. **Display rules were emitted as candidates** → the emit loop skips them.
3. **An egui `Area`'s painter is clipped to its own content rect** → dim overlays must use
   `ctx.layer_painter(LayerId::new(Order::Middle, …))`.
4. **`scan()` returns a flat `Vec<DirEntry>`** with recursive sizes — no parent links. The treemap
   builds its own tree.
5. **The map node graph is pruned on purpose** (`total_bytes / 200_000`, floor 2 MB).
6. **`depth_of` is a separator count**, not a nesting level.
7. **`%LOCALAPPDATA%`-style tokens go through the injectable map** — never `std::env` in that path.
8. **A nested `ui.vertical` inside a right-to-left layout inherits almost no width** and wraps to one
   character per line. The memory meters are single labels because of this.
9. **`with_layout(right_to_left)` inside `ui.horizontal` is a width trap.** It gets the width
   *remaining after the content so far*, so right-aligned buttons drift or vanish — the Review footer
   lost "Quarantine selected" this way for two releases. `allocate_ui_with_layout` shrinks to its
   content, and `ui.columns` inside a header eats the full available **height** (it pushed the
   treemap canvas off-screen). **Fix: read `ui.available_width()` before creating the row and place
   widgets at absolute rects** — `chip_at()` is the reference. Check at 1900 px wide, not just 1264.
10. **Four stacked full-screen dims read as solid black.** When the tour has no hole, paint one veil,
    not `[screen; 4]`.
11. **Pre-existing state can satisfy a tour step.** The quarantine step measured "something is
    staged", so leftover folders from an earlier session skipped it — measure from a baseline taken
    when the tour starts.
12. **An `MsgBox` in an Inno `CurUninstallStepChanged` hangs `/SILENT`** unless guarded by
    `not UninstallSilent`. It hung an unattended uninstall on an invisible dialog.
13. **A missing step target must not trap the user.** If a step's control is not in the layout for
    1.2 s, advance instead of showing a black screen with only "Skip".
14. **The free-space card and the `SIZE`/`STATUS` chips are absolute-positioned on purpose.** Do not
    "tidy" them back into a nested layout — see trap 9.

---

## 9. What is done (do not rebuild)

Everything below is implemented, tested and shipped in `v0.2.0` unless marked.

| Feature | Where | Notes |
| --- | --- | --- |
| Rule engine, tokens, AND matchers | `rules.rs`, `tokens.rs` | 80 rules, first match wins |
| Walk scanner + MFT scanner | `scan.rs`, `mft.rs` | MFT needs elevation; 4.9 s vs 75 s |
| **Exclude from scan** | `scan.rs::scan_any_with`, `cache::exclusions_hash` | both paths honour it; cache key includes it; managed in Rules |
| Quarantine / restore / purge + journal | `executor.rs` | the only module with write power |
| **Multi-drive staging** | `executor.rs::stage_root_for` | unit tested; **no second volume on this machine to prove a real cross-volume rename** |
| Treemap, colour by tier/size, zoom | `layout.rs` + `map()` | |
| Inspection layer (contents, not names) | `inspect.rs` | |
| Local AI via Ollama | `ai.rs` | REVIEW ceiling; gated on free RAM |
| **Cloud AI client** | `cloud.rs` | opt-in, off by default, key in memory, payload preview, 45 s timeout. **Never tested against a live endpoint** |
| Rules editor (disable individual rules) | `rules_pane` | writes `overrides.json` |
| **Guided tour — action-gated, 8 steps** | `TOUR_STEPS`, `tour_overlay` | waits for the real control; spotlight + bubble ease; Escape skips; replay from Rules |
| **Mascot "Kiroku-chan"** | `mascot_*.rgba`, `mascot_frame` | 8 frames, blink/scan/sleep/review/point/wave/done; hide from Rules; reduced-motion aware |
| Sidebar navigation + six summary cards + persistent right rail | `sidebar`, `summary`, `inspector` | replaced the old tab row and status strip |
| Light and dark themes, light by default | `Palette`, `pal()` | |
| **Per-user installer** | `installer/opendump.iss` | Inno Setup, no UAC, uninstall leaves staged data |
| Purge confirmation card | `confirm_modal` | names files, requires acknowledgement |

---

## 10. What is NOT done — and what is unverified

Be precise here. Some of this is "not built"; some is "built but never proven". Do not describe the
second group as working.

**Built, never proven**

- **Cloud AI against a live endpoint.** The client compiles and its payload/parsing are unit tested;
  nobody has pointed it at a real server. The UI says "unverified" in those words.
- **A real cross-volume rename.** `stage_root_for` is unit tested; this machine has one volume.
- **The tour's step 7 happy path** (restore landing on the right control). The *fallback* path — no
  target, auto-advance — has been exercised; the happy path after the baseline fix has not.
- **The Exclude button's click handler.** The engine and the cache invalidation are tested, the UI
  renders; the click that writes `exclusions.json` was never posted successfully in this harness.
- **Any machine but this one.** Windows 11, one GPU, one display scaling.

**Not built**

- **Code signing.** Every artefact warns "Unknown publisher". This is a certificate, not code.
- **A tour GIF for the README.** Recording it needs slow-motion pacing; see §11.
- **Installer in CI.** The workflow builds and tests; Inno is run by hand.
- **Elevated tier — decided against.** `hiberfil.sys`, `pagefile.sys`, the Windows Update cache and
  `WinSxS` are reported as `LOCKED` and never removed: they cannot be deleted, only changed through
  `powercfg`, System Properties, a stopped service or `DISM`, so there is no quarantine, no journal
  and no undo. On this machine the reclaimable total would have been ~0 GB anyway. Do not build it
  without a product reason stronger than completeness.

---

## 11. What is next, in order

**1. Cut `v0.2.1`.** `main` is 4 commits ahead of the release: the uninstaller's plain-language
prompt with an "open folder" button, the MFT dump moving to `%TEMP%`, the rewritten README with
screenshots and logo, and a line-count correction. The released installer still shows the old
message. First action: bump both `Cargo.toml` versions, rebuild both workspaces, rebuild the
installer, zip, tag, release, then verify the asset hashes.

**2. Code signing.** The only thing between "downloadable" and "trustworthy". Needs a certificate
issued to a verified identity; nothing in the codebase can substitute for it.

**3. A live test of the cloud client.** Point it at one endpoint, confirm the request shape and the
verdict parsing, then either promote it in the README or remove the feature. Until then the README
and the UI must both keep calling it unverified.

**4. Multi-drive, proven.** A second physical volume, one folder quarantined on it, restored, and the
journal checked.

**5. README polish still owed** (measured gap audit, 2026-11-06): a "Why this one?" table versus
WinDirStat / WizTree / BleachBit that is honest about maturity, the performance claim stated in prose
rather than buried in a table, and real release/download badges. A demo GIF is the biggest remaining
visual gap — the tour's easing is too fast to capture at 45 ms, so add a `--tour-pace <seconds>` flag
(or record with Windows Game Bar) and re-record.

**6. Smaller items**: a `--cached` re-classify switch for the CLI (the window reuses `scan-cache.bin`,
the CLI cannot); reduce the 10.4 MB binary (the mascot frames are ~1.7 MB of raw RGBA and could be
PNG with a decoder); `docs/` screenshots go stale after UI changes — re-shoot with the flags in §4.

---

## 12. Release recipe (v0.2.0 is the reference)

```bash
cargo build --release                        # core + CLI
cd apps/native && cargo build --release      # the window (separate workspace!)
cd ../.. && ./installer/build.ps1            # Inno Setup -> dist/*-setup.exe
```

Stage `opendump.exe`, `opendump-native.exe`, `rules/rules.json`, `README.md`, `LICENSE`; zip as
`opendump-<version>-windows-x64.zip`; `git tag -a v<version>`; `gh release create` with **both**
assets. Then verify:

- `gh release view` lists both assets as `uploaded`
- the SHA-256 of each downloaded asset matches the local file
- `cargo test --workspace` is green on the tagged commit (CI runs it)

**The zip must contain `rules/rules.json` beside the exes** — the CLI resolves it there; the window
compiles the same file in. A zip without it gives a rule-less CLI.

**Installer details:** `installer/opendump.iss`, Inno Setup 6 (`ISCC.exe`, installed per-user by
winget — `installer/build.ps1` finds it). `PrivilegesRequired=lowest`, into
`%LOCALAPPDATA%\Programs\opendump`. The uninstall removes the programs and **nothing** under
`%APPDATA%\opendump-native`, then offers to open that folder — staged folders there have not been
deleted, and an uninstaller must never decide that.

---

## 13. Environment gotchas

- **Never round-trip source through PowerShell.** `Set-Content -Encoding utf8` writes a BOM and
  `Get-Content`/`Set-Content` mangles non-ASCII. Use the editor tools or Node with explicit `utf8`.
- **PowerShell quoting eats inline Python.** Heredocs (`<<'PY'`) do not work; `python -c` with nested
  quotes is unreliable. Write a `.py` file and run it.
- **GitHub occasionally rejects pushes** with `fatal error in commit_refs_rules` on this repo — no
  rulesets, no branch protection, v1 pushes fine. It is transient: retry, and if the tag will not
  push, `gh release create` can make it.
- **Synthetic clicks are unreliable.** PostMessage with client coordinates works when the window is
  foreground; `SetForegroundWindow` immediately before, and verify with `GetForegroundWindow()`
  first. A click that "does nothing" is usually a focus problem, not a UI bug.
- **egui needs a repaint to show a change.** The tour requests frames only while it is moving, so a
  still screen is expected; do not "fix" it by requesting every frame (that cost 60 fps redraws).

---

## 14. Working with this user

- **Momentum over ceremony.** "go", "yes", "y" mean implement now. Do not gate progress on approval.
- **They check claims.** Every number you state should be measured in this session or marked as
  previously measured, with the machine named.
- **They want the honest ceiling**, not marketing. "3× less RAM" landed better than "4×".
- **They will push back with a better idea.** Take it, verify it, and say plainly when they are right.
- **They install what you build.** A tour run quarantines real folders; an uninstaller touches a real
  profile. Treat every test as production.

---

## 15. Do not

- Do not weaken §5, the executor refusals, or the AI/guide-card ceiling to make a feature easier.
- Do not add a webview front end. A Tauri shell was built and deleted: 9.98 MB exe but **316 MB of
  WebView2** at runtime to draw a window egui already draws in 10.4 MB.
- Do not rename the app-data directories — staged data lives at absolute paths inside the journal.
- Do not edit `rules.json` in only one repo; the two copies are byte-identical and must stay so.
- Do not commit `opencode.json`, `rules/overrides.json`, scan caches, journals, quarantine folders, or
  `dist/`.
- Do not run `purge` or `restore --all` against the user's live profile while testing.
- Do not hand-edit `graphify-out/`; it is generated. Rebuild with `/graphify .`.
- Do not leave a behaviour change undocumented. Counts, the file map, the roadmap and any decision a
  change contradicts all belong in the same commit — this file is the handoff the next agent trusts.
