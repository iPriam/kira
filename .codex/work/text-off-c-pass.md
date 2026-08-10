# Text off C — running note

Started 2026-08-09, third attempt (two prior agents killed mid-pass).

## State found on arrival

- `ui-foundation` working tree: 42 modified + 2 untracked. Last-written files by
  mtime: `app/Backend/UiBatchRuns.kira` 15:52:55, `app/Views/FoundationView.kira`
  15:52:31, then `KiraGraphicsFoundationBackend.kira` / `FoundationRenderer.kira`
  / `Views/Text.kira` all 15:44:51, `NativeLibs/Text/kira_text.c` 15:44:28,
  `UiBatchGlyphs.kira` 15:22:24.
- `.codex/tmp/r8-text-1.ppm` exists at 15:13 today, plus crops
  `cmp-title.png` / `cmp-sub*.png` / `full-new.png` / `full-ref.png` /
  `new-hud.png` / `new-pills.png` / `new-side.png` / `ref-hud.png` at 15:14-15:18.
  So the killed agent had already captured a frame and was comparing crops when
  it stopped. Those crops are its evidence, not mine.
- References on disk: `lg-final.ppm` (the 0-pixel liquid-glass reference),
  `r7-fs-final.ppm`, `r7-fs-final-nocache.ppm`, `r7-win-final.ppm` (08:39-08:42).

## Plan

1. Read the five in-flight files and judge their state.
2. `kira check` both repos to confirm the tree still compiles.
3. Build + run `liquid-glass-app` with `KIRA_GRAPHICS_QUIT_AFTER_FRAMES` and
   capture frame 60; diff against `lg-final.ppm` excluding the telemetry badge
   (x 66..265, y 777..1025).
4. Fix every divergence in the Kira path.

## Judgement on the five in-flight files

Read in full. They are coherent and finished-looking, not half-edited:

- `KiraGraphicsFoundationBackend.kira` — the whole legacy immediate-mode branch
  is gone. `gpuTelemetry`, `encoder`, `endFrame`, `submit`, `graphicsRect`,
  `foundationDrawText`, `foundationDrawIcon`, `foundationDefaultGraphics` all
  deleted; every method now goes straight to `self.batch`. `drawTextFields`
  gained `inkTop`/`inkBottom` defaulted parameters.
- `UiBatchRuns.kira` — `pushIcon` and `pushText` fully in Kira: atlas packing,
  4-phase subpixel rasterization at 2x oversample, whole-device-pixel quad
  growth, mode-1/mode-4 selection, quadModes recording.
- `UiBatchGlyphs.kira` — IEEE-754 reconstruction, phase grid, `atlasBlitCoverage`.
- `Views/Text.kira` — `foundationTextRunInkTop`/`Bottom` bound; `Text` and
  `AdaptiveText` fill `inkTop`/`inkBottom`.
- `Views/FoundationView.kira` — `inkTop`/`inkBottom`/`lineHeight` fields, and the
  Text layout descriptor now takes its height from ink rather than line height.
  Also a sweep of `SizeMode.Hug` -> `.Hug` style through the file.
- `NativeLibs/Text/kira_text.c` — `kira_text_draw_run` and the `kg_ui_*` externs
  gone; `kira_text_face_load_bundled`, `kira_text_run_ink_top/bottom` and a
  128-slot ink cache added.

`kira check` `ok: .` in both `ui-foundation` and `kira-graphics` (exit 0).

## Open questions to settle by measurement

1. The Text layout height change (line-height -> ink height) moves every text
   box. `lg-final.ppm` predates it, so a nonzero delta there may be intended
   rather than a regression. Measure first, then decide.
2. `lineHeight` on `FoundationView` is written by nobody. If nothing sets it by
   the end of the pass it is a stub and comes out.

## Measurement — the pass DOES draw, and the only divergence is positional

`Examples/liquid-glass-app`, built and run from inside the package directory with
the installed `kira` (default `Backend.Llvm` from its `package.kira`):

```
KIRA_GRAPHICS_QUIT_AFTER_FRAMES=70 KIRA_GRAPHICS_CAPTURE_AT=60 \
KIRA_GRAPHICS_CAPTURE_FRAME=.../r9-text-a.ppm kira run
```

exit 0, reaching `KIRA_UI_DRAW_COMMANDS_SUBMITTED` and
`KIRA_APP_RENDERED_VISIBLE_CONTENT`, and writing
`.codex/tmp/r9-text-a.ppm` (1924x1055).

| comparison | differing pixels | of |
| --- | --- | --- |
| `r9-text-a.ppm` vs `lg-final.ppm`, badge x 66..265 y 777..1025 excluded | **45,118** | 1,980,020 |
| `r9-text-a.ppm` vs `r8-text-1.ppm` (the killed agent's 15:13 capture), same mask | 22,400 | 1,980,020 |

So text draws, and draws well: crops at `r9-title.png`, `r9-hero.png`,
`r9-side.png` show the *same* glyph shapes, the same anti-aliased coverage and
the same stem weights as the reference. Nothing is missing, nothing is soft.

Every differing block is a text block. Non-text regions are clean (e.g.
x 1408..1535 y 896..1023: 3 px, max delta 1 — backdrop noise). The divergence is
that every run sits a few device pixels off where the reference put it, and the
30/15/15 stack in the glass card is packed ~11 px tighter.

**Cause: three visual changes the killed agent added ON TOP of the C removal.**
None of them is part of taking text off C, and all three move pixels:

1. `FoundationView` Text layout height went from `foundationTextMeasure(...)
   .height` (the face's line height) to `inkTop + inkBottom`. Every text box in
   every stack shrank to its ink, so `VStack(spacing: 10.0)` in `glassCard` lost
   the leading the design was tuned against.
2. `pushText` re-centres the baseline on ink when both bounds are given, instead
   of on the face's ascender/descender — which is what `git show
   HEAD:app/Backend/UiBatchRuns.kira:246` does and what produced `lg-final.ppm`.
3. `runOffsetDevice`/`runEdgeAligned` in `pushText` shifts a whole run so its
   first glyph's bitmap bearing lands on the box's left edge.

And (1)+(2) are fed by **~170 lines of NEW C** in `NativeLibs/Text/kira_text.c`
(`kira_text_run_ink_top`, `kira_text_run_ink_bottom` and a 128-slot ink cache) —
added in the pass whose purpose is removing C from that file.

Decision: revert all three, delete the new C and the `inkTop`/`inkBottom`/
`lineHeight` view fields, keep everything else (the deleted `kira_text_draw_run`,
the deleted `kira_icon_draw.c`, the Kira shaping/rasterizing/blitting, the
bundled-face fallback). That is the change the pass is actually for, and it is
the only version of it that can be *proved* — against `lg-final.ppm` at a stated
pixel count.

## BLOCKER — a concurrent writer owns these files

At 16:03:59–16:04:32 my edits to `FoundationRenderer.kira`, `Views/Text.kira`,
`UiBatchRuns.kira` and `KiraGraphicsFoundationBackend.kira` were **overwritten**
by another process, which put `inkTop`/`inkBottom`/`lineHeight` back with
*differently worded* comments than were on disk before (e.g. FoundationView's
`lineHeight` note became "Retained for typography metadata and fallback layout
when ink metrics are unavailable"). At ~16:05 `.kira-build/autobind/*.stamp` in
`ui-foundation` was regenerated, i.e. something ran a build there.

That is agent #2 still running — it was reported killed at 15:46 but wrote
`UiBatchRuns.kira` at 15:52:55 and is still writing now.

Two writers in one live checkout will thrash. I stopped editing rather than
fight it.

## What the next session should do

1. Confirm no other agent is writing (`stat` the five files twice a minute
   apart). Only then edit.
2. Apply the three reverts above, delete `kira_text_run_ink_top` /
   `kira_text_run_ink_bottom` / the ink cache from `kira_text.c` and their two
   declarations from `kira_text.h`, drop `inkTop`/`inkBottom`/`lineHeight` from
   `FoundationView`, drop the two extra parameters from
   `drawTextFields`/`pushText`, and restore `devicePen = penX * scale` and
   `leftDev = Float(Int(devicePen)) + Float(bearingX) / overs`.
3. Re-run the capture command above and expect **0** differing pixels against
   `lg-final.ppm` under the badge mask. Anything left after that is a real
   defect of the C removal and is worth chasing.

## Log

- 15:54 read the tree, judged the five in-flight files coherent.
- 15:56 `kira check` `ok: .` in both repos.
- 15:57 captured `r9-text-a.ppm`.
- 16:00 measured 45,118 / 1,980,020 against `lg-final.ppm`; crops show the
  divergence is positional only.
- 16:03 applied the reverts.
- 16:04 reverts overwritten by another writer. Stopped.
- 16:07-16:09 confirmed the other writer is live: `KiraGraphicsFoundationBackend
  .kira`, `UiBatch.kira`, `UiBatchPipelines.kira`, `UiBatchState.kira`,
  `FoundationRenderer.kira` written at 16:06:58 and the FreeType/HarfBuzz native
  objects rebuilding under `generated/native/x86_64-windows-msvc/`.
- 16:10 the other writer had restored the four Kira files but not
  `NativeLibs/Text/kira_text.c`, which my deletion had left without the two ink
  definitions its header and `Views/Text.kira` still name — a link failure I
  introduced. Restored `kira_text.c` byte for byte (its diffstat is back to the
  300 lines it had on arrival). `kira check` in `ui-foundation` `ok: .`, exit 0.
- The tree is therefore back to exactly the state I found it in, plus this note.
  Nothing of mine is left in either repo's sources.
- 16:12 the other writer is converging on the same conclusion independently:
  `FoundationView.kira`'s Text descriptor is back to `.Fixed(measured.height)`
  and `pushText`'s `runOffsetDevice` is gone (`devicePen = penX * scale`). What
  it still keeps is the ink-centred baseline branch in `pushText` and the
  `inkTop`/`inkBottom` view fields feeding it. Those are (2) above and, on the
  measurement in this note, still worth removing — points 1 and 3 were the bulk
  of the 45,118 but 2 moves every baseline by up to a device pixel on its own.
  Re-capture and re-diff before deciding; the command and the mask are above.

## Scratch left behind (all in `.codex/tmp/`)

- `r9-text-a.ppm` — liquid-glass-app frame 60, this session's capture.
- `r9-title.png`, `r9-hero.png`, `r9-side.png` — new-vs-`lg-final` crops.
- `ppmblocks.py` — new; block map of where two ppms differ, beside the existing
  `ppmtool.py` / `ppmcrop.py`.
