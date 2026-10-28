# Sumi — Stable Diffusion prompt pack

Companion to `mascotdesign.md` §9. Targets anime checkpoints, booru tags, one seed, one character.

---

## 1. Read this before generating

**1. Use tags, not sentences.** Anime checkpoints are trained on Danbooru tags. *"a tired young
woman in an oversized blazer"* is a worse prompt than `1girl, solo, tired, half-closed eyes,
oversized blazer`. Tags are comma-separated, lowercase, ordered roughly subject → clothing → pose →
composition → style.

**2. Stable Diffusion cannot write transparency.** `transparent background` is a wish, not a
capability — you get a checkered-looking render or a flat colour. Generate on `simple background,
white background`, then cut her out (§6). Every frame must be cut with the **same** bounding box or
the blink will jitter.

**3. One character, one seed, then img2img.** Text-to-image four times produces four different women.
Generate `idle` once, keep the seed, and derive every other expression from it at 0.35–0.45 denoise
(§5).

**4. Props are the weak spot.** `holding stamp` is not a strong tag — anime models will give you a
closed fist, a phone, or a knife. Two options: accept it and inpaint the stamp once, or generate her
with `holding small object` and composite the stamp as a separate sprite. The second is better for
the design anyway: the stamp's pad colour changes per tier, which a baked PNG cannot do.

---

## 2. Reusable blocks

**Character** (never change between frames):

```
1girl, solo, mature female, short hair, black hair, messy hair, bob cut, ahoge,
half-closed eyes, tired, sleepy, expressionless, small mouth,
grey blazer, oversized blazer, white dress shirt, collared shirt, sleeves past wrists,
red pen, pen on lapel
```

**Style + composition:**

```
upper body, standing, three-quarter view, facing left,
flat color, cel shading, clean lineart, sharp outlines,
simple background, white background
```

**Quality** (keep it short; modern SDXL anime models punish tag spam):

```
best quality, highres
```

---

## 3. Master prompt — `sumi_idle`

**Positive:**

```
1girl, solo, mature female, short hair, black hair, messy hair, bob cut, ahoge,
half-closed eyes, tired, sleepy, expressionless, small mouth,
grey blazer, oversized blazer, white dress shirt, collared shirt, sleeves past wrists,
red pen, pen on lapel, holding stamp,
upper body, standing, three-quarter view, facing left,
flat color, cel shading, clean lineart, sharp outlines,
simple background, white background, best quality, highres
```

**Negative:**

```
(worst quality, low quality:1.4), lowres, jpeg artifacts, blurry,
bad anatomy, bad hands, extra fingers, missing fingers, extra arms, fused fingers,
text, watermark, signature, username, logo,
multiple girls, 2girls, child, loli, chibi, toddler,
sparkle eyes, starry eyes, heavy blush, open mouth smile,
school uniform, sailor collar, cat ears, animal ears, tail,
gradient background, scenery, detailed background, busy background,
3d, realistic, photorealistic, painting, sketch
```

**Settings — SDXL anime** (Illustrious-XL, NoobAI-XL, Animagine XL 4.0):

| Setting | Value |
|---|---|
| Resolution | 832 × 1216 (portrait) |
| Sampler | Euler a, or DPM++ 2M Karras |
| Steps | 28–32 |
| CFG | 5.0–6.5 |
| Clip skip | 2 |
| Hires fix | 1.5×, 12 steps, denoise 0.35, `R-ESRGAN 4x+ Anime6B` |
| Seed | fix it, write it down |

**Settings — SD 1.5 anime** (Anything V5, MeinaMix, Counterfeit):

| Setting | Value |
|---|---|
| Resolution | 512 × 768 |
| Sampler | DPM++ 2M Karras |
| Steps | 25–30 |
| CFG | 7 |
| Clip skip | 2 |
| Hires fix | 2×, denoise 0.4, `R-ESRGAN 4x+ Anime6B` |
| Negative embedding | `easynegative, bad-hands-5` at the front of the negative |

**Pony V6 XL** needs its own quality prefix instead of the block above:
`score_9, score_8_up, score_7_up, source_anime` — and tends to sexualise `mature female`; drop that
tag and keep `1girl, solo, adult`.

---

## 4. Expression variants

Each of these replaces the pose/prop fragment of the master prompt; the character block stays
byte-identical. Generate with img2img from `sumi_idle` (§5) — text-only will change her face.

### `sumi_blink`

```
...character block...,
eyes closed, relaxed brow, neutral expression, holding stamp,
upper body, standing, three-quarter view, facing left, ...
```

Denoise **0.20–0.30** — you are only changing the eyes. Higher and the pose drifts.

### `sumi_squint` (scanning)

```
...character block...,
one eye closed, squinting, holding magnifier, magnifier to eye, head tilt,
upper body, standing, three-quarter view, facing left, ...
```

Denoise 0.40. Expect to fix the magnifier rim by hand; `holding magnifier` lands about half the time.

### `sumi_asleep` (idle 60 s, after purge)

```
...character block...,
sleeping, eyes closed, head tilt, shoulders down, parted lips, holding stamp,
upper body, standing, three-quarter view, facing left, ...
```

Denoise 0.45.

### `sumi_stamp` (scan complete, 400 ms)

```
...character block...,
looking down, arm down, holding stamp, stamping, motion lines,
upper body, standing, three-quarter view, facing left, ...
```

Denoise 0.45. Motion lines are optional — they read as noise at 72 px; prefer the squash animation in
code instead.

### `sumi_empty_hands` (quarantine pane)

```
...character block...,
empty hands, arms at sides, neutral expression, no prop,
upper body, standing, three-quarter view, facing left, ...
```

Denoise 0.45. Generate this one too — it doubles as the base for any future pose, because adding a
prop by img2img is easier than removing one.

---

## 5. Consistency workflow

1. **txt2img** the master prompt at 832 × 1216 until the face is right. Do not accept a "close
   enough" face — every other frame inherits it.
2. **Write down the seed and the checkpoint hash.** Different checkpoint = different woman.
3. For each variant: **img2img**, same checkpoint, same seed, the variant prompt, **denoise 0.25–0.45**
   as noted above.
4. Lock the pose with **ControlNet OpenPose** (or Depth) using the idle frame as the reference, weight
   0.5–0.7. Without it, arms migrate between frames and the frame swap flickers.
5. For character identity across larger changes, **IP-Adapter** with `sumi_idle` as the reference image,
   weight 0.6–0.8. This is the single biggest consistency win after fixing the seed.
6. Run **ADetailer** (`face_yolov8n`, `hand_yolov8n`) on every frame. At 72 px the face is 20 px;
   a mushy face is the whole character failing.
7. Upscale once at the end, not per frame: `4x-UltraSharp` or `R-ESRGAN 4x+ Anime6B` to ~2048, then
   downscale to the 1024 px asset height.

**If hands keep failing:** crop the hands out of frame. The design already hides them in cuffs past
the fingertips — use it. `sleeves past wrists` is both the silhouette signature and the reason you
never have to render a hand.

---

## 6. Cutout and canvas

Stable Diffusion gives you a white background. Remove it:

```bash
# one-time, per frame
pip install rembg
rembg i sumi_idle_raw.png sumi_idle_cut.png
```

```bash
# crop every frame to the SAME box, taken from the idle frame
magick sumi_idle_cut.png -trim info:            # read the geometry: e.g. 612x1024+110+96
magick sumi_idle_cut.png   -crop 640x1024+96+96 +repage sumi_idle.png
magick sumi_blink_cut.png  -crop 640x1024+96+96 +repage sumi_blink.png
magick sumi_stamp_cut.png  -crop 640x1024+96+96 +repage sumi_stamp.png
magick sumi_idle.png -resize x1024 sumi_idle.png
```

The point of the fixed crop is **frame stability**: `-trim` per frame produces a different box for
each and the blink will jump 3 px sideways at 72 px display. Trim once, crop all.

Then check the alpha edge on a dark background — straight alpha leaves a pale halo on `#131011`.
Premultiply on load in code, or bake the 1 px ink contour described in `mascotdesign.md` §5.4.

---

## 7. What "good" looks like

Reject a frame if any of these are true:

- She does not pass the silhouette test at 72 px (solid fill: bob, big shoulders, cuffs).
- Two or more frames differ in head position by more than ~4 px at 1024.
- Red appears anywhere except the pen.
- The expression is carried by the mouth only.
- Hands are visible and wrong. Crop them; the sleeves are the design.
- The white shirt merges with the background on the light theme.

---

## 8. Model shortlist

| Model | Family | Why |
|---|---|---|
| Illustrious-XL v0.1 / v1.0 | SDXL | Best tag adherence for adult anime characters; clean lineart |
| NoobAI-XL | SDXL | Stronger danbooru coverage, good at `tired`/`half-closed eyes` |
| Animagine XL 4.0 | SDXL | Friendlier default style, softer lines |
| Anything V5 | SD 1.5 | Proven, fast, runs on 4 GB VRAM |
| MeinaMix | SD 1.5 | Softer, more illustrative; weaker at `oversized blazer` |

Avoid realistic/photoreal merges — the design depends on flat cel shading and a hard outline.
