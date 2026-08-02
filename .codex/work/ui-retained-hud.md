# The HUD has a retained stream of its own

The graphics overlay refreshes ten times a second, and every refresh used to
rebuild the entire interface: the widget builder ran, the retained tree
reconciled, the layout pass ran, and every application quad was re-emitted, all
so a counter could change. On the native-4K liquid glass app that cost a
metronome of **4.2–4.8 ms frames every ~84 frames** against a 1.18 ms median —
visible in a per-frame trace as a perfect periodic spike.

The overlay is now emitted into a stream nothing else writes
(`uiBatchHudStream()`, one past the last glass layer), so refreshing it rewrites
that buffer's ring slot and leaves every UI stream's vertices, quad count and
slot exactly as the last full frame left them.

The periodic spike is gone. On a quiet machine, over 2700 steady-state frames of
a 3000-frame release run, frames costing over 3.5 ms of CPU fall from **35–38,
averaging 7.1 ms and evenly spaced ~81 frames apart**, to **5–7, averaging 3.8 ms
and with no periodicity at all**. p99 goes 6.89 ms → 3.66 ms, max 11.4 ms →
5.6 ms.

Timed against the refresh itself rather than the frame — 70 refresh events per
run, three runs — the work behind one goes from **7.17–8.46 ms to 1.19–1.35 ms**,
while a plain re-presented frame is untouched at 1.02–1.04 ms either way.

## What made it tractable

Stream slots have **fixed indices**. `uiBatchCreate` fills `streams` with
placeholders up to `uiBatchStreamSlots()` and each slot allocates its buffer on
first use (`uiBatchCreateStream`), so the HUD sits at a known index whether or
not glass ever stacks that deep — and no branch on "is this the HUD" is needed
in the emit loop, because `streamIndex` already selects it. `uiBatchBeginHud`
saves the mirrored hot fields, points them at that slot and claims its ring slot;
`uiBatchEndHud` hands them back. Both work mid-walk and on a frame that does
nothing else.

Two consequences fall out of the HUD being drawn last, which is where the
composed overlay drew before it had a stream: it is never inside a glass capture
(captures hold streams `0..layer`), and its quads never need to record glass
occupancy, since every glass decision of the frame has already been taken.

The immediate Sokol path keeps composing the overlay into the tree. It has no
retained batch to leave alone.

## What the spike was actually made of

Isolating the HUD was necessary and not sufficient — a HUD refresh is ~250 glyph
quads, and emitting a quad was expensive for two reasons worth remembering.

**Most of every vertex was a copy of the vertex beside it.** Six of the values a
vertex needs vary across a quad's four corners and eighteen do not, so a
per-vertex buffer wrote those eighteen four times — and a screen of text is one
quad per glyph. The buffer now holds one record per quad carrying each varying
value as its two extremes, and the vertex stage picks with `mix` against a weight
of exactly 0 or 1, which returns an operand unchanged. The clip position is
computed host-side and stored the same way rather than derived in the stage from
a viewport, so the bytes reaching the rasterizer are the ones the per-vertex
buffer held and the framebuffer is unchanged rather than merely close.

**Half of what was left was zeros.** The material block is 24 of the record's 54
floats and only mode 3, liquid glass, reads any of it, so only mode 3 writes it;
the slot keeps whatever the last quad to occupy it left there, which the other
modes never look at. A quad costs **30 float writes where it cost 192**, and a
stream's ring shrank from 3 MiB to 885 KiB.

The generic `uiBatchMslSource` went with this: nothing called it — the pipeline
is compiled from the `ksl!` artifact — and its `UiVertex` had been a 24-float
struct for long enough to be actively misleading about the layout.

**A field of a recovered state is not a load.** The runtime re-derives and
type-checks the payload address on every access, so a loop that reads one costs
a call per read. The glyph hash probe read two arrays several times per
character that way, and the slot read that follows it took eight fields of one
`UiGlyphSlot` one at a time. Both now take a share of the array once per string
(a count, not a copy, since `0213fe5`) and read the slot whole, re-taken after a
rasterization appends. Worth 0.56 ms of the 2.02.

**The runtime archive was built unoptimized.** `kira_rt_native_state_box_payload`
runs once per field access on a recovered state and once per vertex write, and
without inlining each is a chain of un-inlined helpers. `kira-native-bridge` and
`kira-runtime-abi` now compile at `opt-level = 3` in the dev profile, for the
same reason `kira-vm-runtime` does: they are not tooling that runs once, they are
the inner loop of every program the compiler produces. That alone moved the HUD
refresh from ~10 ms to ~3.5 ms. Recovery also stopped rebuilding two `Layout`s to
recover an offset fixed at allocation (`payload_offset`, pinned against
`box_layout` by a test).

## Where the last 1.27 ms is

Text emission is **93%** of a refresh — building the eleven views is 6% and the
layout pass does not register. Within emission, **63%** is now the glyph hash
probe and the slot read beside it, and the vertex writes that used to rival them
are down to under a tenth.

That remaining 63% is `kira_rt_array_slot`: indexing a Kira array calls the
runtime, and the probe indexes three of them per character. It is not reachable
from Kira source — the arrays are already held directly rather than through the
recovered state — so the next lever is the compiler, not the compositor. Sizing
it first would be wise: predicting the per-quad record at 0.4 ms and measuring it
at a tenth of that is what this section is here to prevent a second time.

## Measuring it

`KIRA_METAL_BENCH_FRAMES` sets the benchmark's frame count, which the 180-frame
default is too short for: a run has to contain several periods of whatever it is
looking for. Native 4K is 1920x1080 points at `KIRA_METAL_OFFSCREEN_SCALE=2`.

```
cd Examples/liquid-glass-app
env KIRA_METAL_OFFSCREEN=1 KIRA_METAL_OFFSCREEN_SCALE=2 \
    KIRA_METAL_BENCH=1 KIRA_METAL_BENCH_DETAIL=1 KIRA_METAL_BENCH_FRAMES=3000 \
    ./app/.kira-build/main
```

Read the **CPU** column of the detail trace, not the frame column: a frame is
GPU-paced at ~1.18 ms while its CPU work is ~0.15 ms, so `frame[i]` is mostly
waiting and lands one index after the CPU cost that caused it. Count frames over
a threshold rather than reading p99 — this machine carries a periodic ~3 ms GPU
hitch on a ~12 ms wall-clock period that is present at every resolution and in
every build, ours or not, and it moves a percentile around far more than it moves
a count.

Compare against a build of the *previous* source, run alternately with the new
one so contention lands on both. Absolute numbers taken minutes apart on a
machine running Spotlight, Time Machine or a screen-sharing session are not
comparable to each other.
