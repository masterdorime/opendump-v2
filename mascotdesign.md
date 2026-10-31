# Mascot design — the disc girl

**Status:** character chosen from candidates (flat CD-girl sheet); app integration is built and
running with a placeholder; **no final art yet**.
**Target:** `apps/native` (v2, egui 0.29).
**Working name:** none. The UI says `DRIVE GUIDE`; pick a name before the first real asset, because
it lands in filenames and the settings key.

---

## 1. Why she exists

1. **The empty and idle states are the weakest screens.** She owns the rail space under the
   inspector, which was previously dead.
2. **The disc explains the product.** This app maps a disk. A platter behind her ear says *storage*
   in a way a folder icon never will.
3. **She is the tone.** A tool that permanently deletes files should feel calm and precise. She is
   deadpan, not cheerful.

## 2. Non-negotiables

| # | Rule | Reason |
|---|---|---|
| 1 | **She lives in the right rail's guide card.** No floating overlay. | A corner overlay covered the inspector's buttons and review rows at every size tried. |
| 2 | **She goes still during the purge confirmation.** No spin, no copy, no celebration. | A cheerful character beside "delete 12.4 GB permanently" reads as endorsement. |
| 3 | **She never receives clicks.** | The card is layout, not a control. |
| 4 | **She carries no information that exists nowhere else.** | Decorative art must not be an accessibility dependency. |
| 5 | **She can be turned off in one click, and the choice persists.** | Not everyone wants a character in a disk tool. |
| 6 | **No text baked into the artwork.** | Text dies at 72 px and cannot be translated. |
| 7 | **Her red is never a signal.** One thin pinstripe at most, never a filled badge. | `#FF2D55` already means `LOCKED` in the map. Two reds meaning different things is a bug. |

## 3. Character

### 3.1 Identity

| Field | Value |
|---|---|
| Role | The drive, personified. Not a clerk, not an idol — a machine with a face. |
| Temperament | Deadpan, patient, faintly bored. Never winks, never celebrates. |
| Age read | Late teens at the youngest; the current sheets read younger and need lengthening. |
| Signature | A CD platter where an ear would be, repeated as disc joints on the shoulders and cuffs. |
| Motif | Fragmentation — pastel shards in the hair, like a disc that has been read in pieces. |

### 3.2 Proportions

| Measure | Value | Note |
|---|---|---|
| Head : total | **1 : 3.5** | The candidate sheets are closer to 1 : 2.8. Lengthen the body, not the head. |
| Eye line | 55 % down the head | Current art sits at ~45 %, which reads younger. |
| Silhouette | Disc + bob + wide jacket | Those three shapes must survive at 40 px. |
| Hands | Never rendered | Every candidate sheet has malformed hands. The sleeves are the design. |

### 3.3 Palette

Six values, not four — the disc needs its prism, but only two bands of it.

| Role | Hex | Where |
|---|---|---|
| Paper | `#F2F1F4` | Hair, jacket |
| Ink | `#17161A` | Outline, underlayer, disc hub |
| Silver | `#C9C7D0` | Disc body, shading planes |
| Prism A | `#8FE3E0` | One shard band (cyan) |
| Prism B | `#F2A8D2` | One shard band (pink) |
| Accent | `#E02B3C` | One pinstripe. Matches the UI accent — and must never be confused with LOCKED |

**Rule: two prism bands, not five.** The candidate has mint, lilac, cyan, pink and yellow. Five
pastels in a UI whose only saturated colours are tier-coded makes the map look like her background.

### 3.4 Silhouette test

Fill her solid black at 40 px. A stranger must identify: a bob, a disc at the ear, a wide angular
jacket. If the shards are the only thing that survives, the design is wrong.

### 3.5 Expression sheet

| # | Key | Face | Body | Used when |
|---|---|---|---|---|
| 1 | `idle` | Deadpan, half-lidded, small mouth | Arms at sides | Default |
| 2 | `blink` | Eyes closed | Unchanged | 120 ms, every 4–7 s |
| 3 | `scan` | Eyes down, brow slightly drawn | Disc spinning | Scanning |
| 4 | `review` | One eyebrow up | Head tilt | Review pane, or a REVIEW folder |
| 5 | `asleep` | Eyes closed, mouth open | Shoulders dropped | Idle 60 s |
| 6 | `disc` | — | **Separate texture** | Rotated in code during a scan |

The disc is its own image so scanning costs a rotation, not a new drawing.

## 4. Behaviour

### 4.1 States

| Window state | Card | Art |
|---|---|---|
| No scan | Normal | `asleep` after 60 s |
| Scanning | Normal | `scan`, disc rotating 1 rev / 1.2 s |
| Scan done | Normal | `idle` |
| REVIEW folder selected | Normal | `review` |
| Purge confirmation open | Normal | `idle`, rotation stopped |
| Toggled off | Card hidden | — |

### 4.2 Copy

One line maximum, in her voice: a machine that counts, not a companion. No exclamation marks.

| State | Line |
|---|---|
| Empty | "Nothing yet." |
| Scanning | "Reading {files} names…" |
| Done | "Counted." |
| Review folder | "This one is yours." |
| No rule matched | "No rule knows this folder." |

The tier, size and reason in the guide card come from the rule engine. She may restate them; she
may never invent a verdict — the same ceiling the local AI has.

### 4.3 Motion

| Motion | Spec |
|---|---|
| Disc spin | 1 revolution / 1.2 s while scanning, ease-out to stop |
| Blink | swap frame, 120 ms, random 4–7 s |
| Breathing | 1 px vertical, 3.2 s |
| Enter | fade 150 ms |

No motion during the purge card. None under reduced-motion.

## 5. Placement

The right rail is always visible: drive or folder inspector at the top, **the guide card pinned at
the bottom**, above the window edge.

- **Title:** `DRIVE GUIDE`, 9.5 pt, dim.
- **Headline:** tier + exclusive size, in that tier's colour.
- **Body:** the matched rule's `reason`, verbatim.
- **Action:** one tier-appropriate line — *Safe to clear* / *Your call* / *I would keep this one* /
  *Locked, not offered*.
- **Portrait:** 72 pt by default; `--mascot <px>` to review another size.
- **Rail width:** 304 pt, already present — the card costs no layout.

## 6. Assets

### 6.1 Contract

| File | Size | Alpha | Notes |
|---|---|---|---|
| `mascot_idle.png` | 1024 px tall | transparent | **Mandatory.** Deadpan |
| `mascot_blink.png` | same canvas | transparent | **Mandatory.** Eyes only |
| `mascot_disc.png` | square, 512 px | transparent | **Mandatory.** The disc alone, centred and circular, so it can rotate |
| `mascot_review.png` | same canvas | transparent | Optional |
| `mascot_asleep.png` | same canvas | transparent | Optional |

Rules:

- Identical canvas and anchor across frames.
- Flat cel only: no gradients, no rim light, no rendered shadows. The candidate sheets are AI
  renders with soft shading; they must be flattened, not traced.
- 1 px ink contour baked in, so the white jacket survives the light theme (`#F7F5F5`).
- Crop or erase hands. Do not attempt to fix them.

### 6.2 Placeholder

`apps/native/assets/mascot240x315.rgba` — a flattened cutout of the chosen candidate, raw RGBA at
240×315, loaded exactly like the logo. It proves the pipeline; it is not shippable.

## 7. Implementation (as built)

```
consts    MASCOT_RGBA/W/H   raw RGBA, include_bytes!, no decoder dependency
fields    mascot_art: Option<egui::TextureHandle>
          mascot_image: bool          --mascot-image
          mascot_preview: Option<f32> --mascot <px>
views     guide card pinned under the inspector (fn inspector)
flags     --pane <name>, --zoom <path>
```

The card computes its copy from the selected candidate: tier label, exclusive size, the rule's
reason, and one action line. No match produces "No rule matched" rather than a guess.

### 7.1 Traps already hit

| Trap | Why it bites |
|---|---|
| Floating overlay | Covered the inspector's buttons and 2½ review rows at every size |
| Bottom dock | Cost 72 pt of map height and still needed the rail for Review/Rules |
| Hand-edited settings | `save_light()` rewrites `settings.json` whole; a mascot key would be wiped. Fix it before adding the toggle |
| `pixels_per_point` in sizes | 125 % scaling turns a 72 pt card into 90 px and clips the rail |

## 8. Accessibility

- **Reduced motion:** no spin, no blink, no breathing. A still image.
- **Toggle:** in the Rules pane, persisted, labelled "Show the mascot".
- **Decorative:** no tooltip may contain unique information.
- **Contrast:** the card never carries a tier colour that is not also in the row text.
- **No audio**, ever.

## 9. Prompt pack

Target the flat candidate, not the painterly one. One seed, image-to-image from `idle`.

**Style line:**

> flat cel-shaded anime, 2 px line weight at 1024 px, limited palette of white, silver, black, two
> pastel prism bands and one red pinstripe, hard ink outline, transparent background, upper body,
> deadpan expression, half-lidded eyes, white bob, CD disc hair ornament, oversized white jacket
> with disc buttons, black underlayer, no hands visible, no text

**Idle:** neutral stance, arms at sides, small flat mouth, no smile.

**Blink:** identical, eyes closed.

**Scan:** eyes down, head tilted toward the content, disc implied spinning.

**Negative:**

> smile, wink, blush, sparkle eyes, open mouth, school uniform, ribbon, pleated skirt, rainbow,
> five-colour gradient, hands, extra fingers, text, watermark, 3d, realistic, painterly, soft
> shading, rim light, background scenery

## 10. Acceptance

- [ ] 40 px silhouette test passes: bob, disc, jacket.
- [ ] Two prism bands at most; the rest white/silver/black.
- [ ] Reads on `#131011` and on `#F7F5F5` without a halo.
- [ ] No hands in any frame.
- [ ] Disc is a separate square texture and rotates cleanly.
- [ ] Card copy never states a verdict the rules did not produce.
- [ ] Purge confirmation shows her static.
- [ ] `cargo test --workspace` stays green.

## 11. Open questions

1. **Name.** None yet. It lands in filenames, the settings key and the card title.
2. **Tone.** The flat candidate is deadpan; the painterly one is soft. Those are different products.
   This spec assumes deadpan.
3. **Palette.** Six values is already a compromise. If the map starts to feel grey next to her, cut
   the prism to one band.
