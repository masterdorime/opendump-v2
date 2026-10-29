# Sumi — mascot design

**Status:** design agreed 2026-10-29 · **implementation not started** · art is a placeholder until real
PNGs arrive.
**Target:** `apps/native` (v2, egui 0.29) — not v1. v2 has no image pipeline today, so this document
also specifies the one being added.
**One-line:** a night-shift auditor who counts your disk, stamps her verdicts, and is asleep by the
time you read the report.

---

## 1. Why she exists

1. **The empty screen is the weakest screen.** Before a scan, after a scan, and during a 90-second
   scan there is nothing to look at. That is where she lives.
2. **She explains the product without a tooltip.** A stamp and a magnifier say *classified by rule*,
   which is the whole engine, faster than any onboarding copy.
3. **She is the tone.** A tool that permanently deletes files should feel calm and precise, never
   excited. A tired colleague is a trust signal; a cheering mascot is not.

## 2. Non-negotiables

These are safety rules, not style preferences. A change that breaks one is a regression.

| # | Rule | Reason |
|---|---|---|
| 1 | **She is never drawn over data.** She has a dock, not an overlay. | A corner overlay covered the inspector's buttons and review rows at every size tried. |
| 2 | **She goes still during the purge confirmation.** No motion, no copy, no celebration. | A cheerful character beside "delete 12.4 GB permanently" reads as endorsement of an irreversible act. |
| 3 | **She never receives clicks.** | The dock is layout, not a control. |
| 4 | **She carries no information that exists nowhere else.** No tier colour, no byte count, no state is readable only from her. | Decorative assets must not be an accessibility dependency. |
| 5 | **She can be turned off in one click, and the choice persists.** | Motion and character art are not for everyone. |
| 6 | **No text baked into the artwork.** | Text dies at 62 px and cannot be translated. |

## 3. Character

### 3.1 Identity

| Field | Value |
|---|---|
| Name | **Sumi** (墨, "ink") |
| Role | Night auditor of the drive. Counts, stamps, files. Never deletes. |
| Temperament | Precise, unhurried, faintly amused. Not shy, not bubbly, not clumsy. |
| Age read | Adult colleague. Never a child — this tool destroys data. |
| Signature | Sleeves past the fingertips, blazer two sizes too big, one strand of hair permanently out of place. |
| Prop | A rubber stamp. Its pad is the only place a tier colour is allowed to appear on her. |

### 3.2 Proportions

| Measure | Value | Note |
|---|---|---|
| Head : total height | **1 : 3.5** | Not chibi. Chibi undercuts trust on a destructive screen. |
| Eye line | 55 % down the head | Low eye line reads calm; high reads startled. |
| Shoulders | Narrow, sloped | The oversized blazer supplies the width, not the body. |
| Cuffs | Past the fingertips | The silhouette signature. Must survive at 72 px. |
| Pose | Standing, weight on one leg, stamp held against the chest | Symmetrical T-pose is forbidden. |

### 3.3 Palette

Four values only. Deliberately fewer than the UI so she never competes with the tier colours.

| Role | Hex | Where |
|---|---|---|
| Hair / outline | `#1A1416` | Hair, brow, the 1 px outer contour |
| Garment | `#4D5A6E` | Blazer |
| Shirt / paper | `#F0ECE8` | Collar, cuffs, stamp body |
| Accent | `#E02B3C` | The pen clipped to her lapel — **nothing else** |
| Skin | `#F2D6C6` | Face, hands |

The UI accent is `#E02B3C` (dark theme). Light theme uses a deeper red for contrast; **her pen stays
`#E02B3C`**, because a pen is not a UI control and does not need to pass contrast against text.

### 3.4 Silhouette test

Fill her in solid black at 72 px. A stranger must still identify: bob haircut, oversized shoulders,
cuffs past the hands, something rectangular held at the chest. If a pose fails this, it is wrong no
matter how good the render looks at 1024 px.

### 3.5 Expression sheet

Seven drawings. Six are optional if art budget is tight; **idle and blink are mandatory**.

| # | Key | Face | Body | Used when |
|---|---|---|---|---|
| 1 | `idle` | Half-lidded, single highlight, small flat mouth | Stamp at chest | Default; empty state |
| 2 | `blink` | Eyes closed, relaxed brow | Unchanged | 120 ms every 4–7 s |
| 3 | `asleep` | Eyes closed, mouth slightly open, head tilted | Shoulders dropped | Idle 60 s · after purge |
| 4 | `squint` | One eye through the magnifier | Magnifier raised | Scanning |
| 5 | `brow` | One eyebrow up, mouth unchanged | Stamp offered outward | Review list open |
| 6 | `stamp` | Eyes down at the work | Arm down, stamp in contact | 400 ms on scan completion |
| 7 | `empty_hands` | Neutral, eyes forward | Both hands empty, no stamp | Quarantine view |

Rule: **expression is carried by the eyes and the eyebrows, never by the mouth alone.** At 72 px the
mouth is two pixels.

---

## 4. Behaviour

### 4.1 State machine

`MascotMode` is derived, never stored:

| Window state | Dock | Art height | Pose |
|---|---|---|---|
| No scan in memory | **132 pt** | 120 px | `idle` / `asleep` after 60 s |
| Scan running | **132 pt** | 120 px | `squint` |
| Scan just finished (≤ 2 s) | 72 pt | 62 px | `stamp` |
| Scan present, any pane | 72 pt | 62 px | `idle` |
| **Purge confirmation open** | 72 pt | 62 px | still, no motion, no copy |
| User toggled her off | 0 pt | — | — |

### 4.2 Copy

One line maximum, only in these four states. Voice: a colleague at 3 a.m., not a marketing mascot.
Sentence case, no exclamation marks, no emoji.

| State | Line |
|---|---|
| Empty | "Nothing yet. I'll wait." |
| Scanning | "Reading {files} names…" |
| Scan done | "Counted. Twice." |
| Review | "This one's yours." |

Forbidden: anything about deletion, anything congratulating the user, anything that implies she will
do the deleting.

### 4.3 Motion

| Motion | Spec | Driver |
|---|---|---|
| Breathing bob | 1 px vertical, sine, 3.2 s period | `ctx.input(\|i\| i.time)` |
| Blink | swap to `blink`, 120 ms, random 4–7 s | time + a stored next-blink deadline |
| Scan start lean | 2 px toward the content, 250 ms ease-out | state transition |
| Stamp contact | scale 1.05× for 400 ms, then back | state transition |
| Enter / leave | fade 150 ms + 6 px rise | mode change |

No motion when `Hidden`. No motion at all under reduced-motion (see §8).

---

## 5. Placement

**She has a card in the right rail, not a floating corner.** A bottom-right overlay was built and
deleted (it covered the inspector's buttons and review rows at every size), and so was a bottom
dock (it cost 72 pt of map height and still needed a second home in the rail).

### 5.1 The guide card

The right rail is always visible: drive or folder inspector at the top, **Sumi's card pinned at the
bottom**, above the window edge.

- **Title:** `SUMI · DRIVE GUIDE`, 9.5 pt, dim.
- **Headline:** tier + exclusive size, in that tier's colour.
- **Body:** the matched rule's `reason`, verbatim. Never prose she invents.
- **Action:** one tier-appropriate line — *Safe to clear* / *Your call* / *I would keep this one* /
  *Locked, not offered*.
- **Portrait:** 72 pt beside the text, `--mascot <px>` to review another size.
- **No match:** "No rule matched · {size}" and a pointer at Inspect contents.

The rail is a `SidePanel`, 304 pt wide, on every pane. Cost: zero — it already existed for the map.

### 5.2 Growth

There is nothing to grow: the card is the same size in every state, and at the 600 pt minimum
window it stays inside the rail. If the rail ever collapses on a small window, the card collapses
with it and she disappears rather than overlapping content.

### 5.3 Never

- Over the review list, the treemap, the inspector, or any button — the rail makes this structural.
- As a floating overlay above the content, at any size. That decision is settled; do not re-open it
  without a screenshot of Review and Rules.
- Spoken or animated during the purge confirmation. She goes still; the card owns the screen.

### 5.4 Light and dark

One asset serves both. On light backgrounds she gains a **1 px `#1A1416` outer contour** and a soft
warm shadow (`#000000` at 12 % alpha, 8 px blur, 2 px y-offset) so the off-white shirt does not merge
with the `#F7F5F5` background. Bake the contour into the PNG; do not draw it in code.

---

## 6. Assets

### 6.1 Contract

| File | Size | Alpha | Notes |
|---|---|---|---|
| `sumi_idle.png` | 1024 px tall, ~512 wide | transparent | **Mandatory.** Base pose |
| `sumi_blink.png` | same canvas | transparent | **Mandatory.** Identical framing; only eyes change |
| `sumi_stamp.png` | same canvas | transparent | Optional; arm down |
| `sumi_asleep.png` | same canvas | transparent | Optional; head tilt |
| `sumi_squint.png` | same canvas | transparent | Optional; magnifier |
| `sumi_empty_hands.png` | same canvas | transparent | Optional |

Rules:

- **Identical canvas and anchor** for every frame. Frame swaps must not shift her by a pixel.
- Flat cel shading, no gradients on skin, no texture patterns — both mush at 72 px.
- Line weight ≈ 2 px at 1024, i.e. ~0.35 px at 72. Keep contours ≥ 3 px at source.
- No drop shadow in the file; shadow is drawn in code.
- No text, no UI chrome, no background, no border.

### 6.2 Placeholder

Until real art lands, ship `sumi_idle.png` as a procedurally drawn flat silhouette (same proportions,
`#4D5A6E` body, `#1A1416` outline, transparent) at the same 1024 px canvas. It exists to prove the
pipeline, and the file is replaced without touching code.

### 6.3 Delivery checklist

- [ ] Transparent background confirmed (not white).
- [ ] Canvas identical across frames.
- [ ] Silhouette test passed at 72 px.
- [ ] Reads on both `#131011` and `#F7F5F5`.
- [ ] No red anywhere except the pen.
- [ ] PNG optimised, each under ~150 KB.

---

## 7. Implementation

### 7.1 Dependency

`eframe 0.29` is already pinned. Add:

```toml
egui_extras = { version = "0.29", features = ["image"] }
```

That pulls in the `image` crate, which decodes the PNGs. Nothing else is added; no webview, no
runtime asset folder.

### 7.2 Code shape

All in `apps/native/src/main.rs`, consistent with the one-file rule.

```
consts    SUMI_IDLE / SUMI_BLINK / SUMI_STAMP: &[u8] = include_bytes!("../assets/sumi_*.png")
          SUMI_FULL = 180.0 · SUMI_PEEK = 72.0
types     enum MascotMode { Hidden, Full, Peek }
logic     mascot_mode(&self) -> MascotMode        // derived from scan state + pane + toggle
          next_blink(&mut self, now: f64)
views     draw_sumi(&mut self, ui, mode)          // reserves rect, paints texture, no Sense
```

Loading: decode once at startup into `ColorImage`, `ctx.load_texture("sumi_idle", …,
TextureOptions::LINEAR)`. 1024 px source covers 180 px at 5.7× for HiDPI; no mipmaps needed.

### 7.3 Settings persistence — a real trap

`save_light()` currently rewrites `settings.json` as `{"light": …}`, discarding every other key. Adding
`"mascot"` without fixing this means toggling the theme wipes the mascot preference and vice versa.
Replace the pair with a small settings struct (or read-merge-write) so both survive:

```json
{ "light": false, "mascot": true }
```

Default `mascot: true` (she is the point), one click to remove her.

### 7.4 Preview flag

`opendump-native --mascot-preview` opens the window and cycles `Full → Peek → Hidden` on a key press,
without scanning. Without it, every visual check costs a 90-second scan.

### 7.5 Traps

| Trap | Why it bites |
|---|---|
| Drawn in an `Area` | An `Area`'s painter is clipped to its own content rect — the same family as the dim-overlay bug (§6.3 of `AGENTS.md`) |
| `Sense::click()` on the peek | Steals treemap clicks |
| Mixing `pixels_per_point` into the size | On 125 % scaling she becomes 225 px and covers the map; size in points, not pixels |
| Re-decoding the PNG per frame | 1024² decode on the UI thread, ~30 ms hitch |
| Frame anchor drift | Blink shift of 1 px is visible at 72 px as a jitter |
| Alpha fringe | PNG straight-alpha over a dark background leaves a halo; premultiply on load |

---

## 8. Accessibility

- **Reduced motion** (`prefers-reduced-motion` equivalent; on Windows, the animation setting in
  Settings → Accessibility): no bob, no blink, no lean. She becomes a still image.
- **Toggle** in the Rules pane, persisted, labelled "Sumi" with the description "show the mascot".
- **Decorative:** no tooltip may contain unique information; screen-reader users lose nothing.
- **Contrast:** she is never the only signal for a tier; the row text and colour dot remain.
- **No autoplay audio.** She never speaks aloud.

---

## 9. Prompt pack

Use one generator session, one seed, and **image-to-image from `sumi_idle.png`** for every other
expression. Four independent generations will not produce one character.

**Style line (append to every prompt):**

> flat cel-shaded anime, limited palette of four colours plus one red accent, clean 2 px line weight
> at 1024 px, soft rim light, transparent background, upper body, adult woman, tired half-lidded eyes,
> oversized blazer with sleeves past the fingertips, short messy bob, rubber stamp held against the
> chest, no text, no logos, front three-quarter view facing left

**Idle:**

> character sheet, neutral standing pose, weight on one leg, calm expression, single eye highlight,
> muted slate blazer, off-white shirt, ink-black hair, small red pen on the lapel

**Blink:**

> identical character, identical framing and pose, eyes fully closed, relaxed brow

**Squint (scanning):**

> identical character, holding a round magnifier to one eye, head tilted, the other eye half-lidded

**Asleep (idle):**

> identical character, eyes closed, head tilted, shoulders dropped, mouth slightly open, still
> holding the stamp

**Negative prompt:**

> text, watermark, signature, extra fingers, sparkle eyes, blush overload, chibi proportions, bright
> saturated background, gradients on skin, motion blur, multiple characters, school uniform, cat ears

**Consistency workflow:** generate idle at 1024 px → keep the seed → img2img at 0.35–0.45 strength
for each expression → check the silhouette test → export PNG with alpha → verify the canvas is
identical across frames.

---

## 10. Acceptance

The feature is done when all of these hold:

- [ ] `cargo test --workspace` green, no new warnings.
- [ ] `--selftest` output unchanged.
- [ ] With her on and off, the treemap, review list, inspector, quarantine and purge flows are
      pixel-identical outside her reserved rects.
- [ ] Purge confirmation shows no trace of her.
- [ ] Clicking a tile under the peek still selects that tile.
- [ ] Toggling theme and toggling mascot do not erase each other.
- [ ] 125 % and 150 % display scaling: she stays 180 / 72 points.
- [ ] `--mascot-preview` shows all three modes without a scan.
- [ ] Exe growth ≤ 500 KB with real art.
- [ ] `AGENTS.md` §2 and the README note are updated in the same commit.

---

## 11. Open questions

1. **Name.** "Sumi" is a proposal, not a decision. It appears in filenames and the toggle label, so it
   is cheap to change now and annoying later.
2. **Sleep at 60 s** — is a sleeping mascot charming, or does it look like the app has crashed?
   Test it in `--mascot-preview` before committing.
3. **Does she belong in v1 at all?** v1 is the shipped product; the same PNGs would drop into the
   renderer in an hour. Not in scope here, but the asset contract above is deliberately portable.
