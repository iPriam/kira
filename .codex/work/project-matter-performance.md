# Making Project Matter run

The Project Matter editor renders its full UI on screen, and one frame costs
**43 ms** — 33 ms when the toolchain is built release, which is what a user
gets. It started this work at 21.9 seconds a frame and did not run at all. The
target is **2 ms on the native engine** (the UI gets a tenth of a 16 ms budget;
the game takes the rest) and **60 fps on the VM**, where a frame is 2.4 s
today.

Everything below is measured, not estimated. Where something was tried and
reverted, the measurement that killed it is recorded — two of the seven
performance attempts made things worse.

## What to do next

**Enum boxes are now what a native frame is made of.** Sampled after
copy-on-write landed, 400 offscreen frames on a debug-built runtime:
`kira_rt_enum_clone` 236 samples and `kira_rt_enum_free` 209, ahead of the
malloc/free machinery they drive (~700) and well ahead of everything array
(~350, mostly the one header allocation a copy still makes). A payload-less
variant already costs nothing — it lives in its handle since `c656a0e` — so
what is left is every *payload-carrying* enum being a heap box that a read
clones and a drop frees.

Give the box a share count, exactly as an array's item block has one:
`kira_rt_enum_clone` becomes an increment, and nothing has to become unique
because **an enum box is never written through**. A variant is replaced whole,
which allocates a new box; there is no `slot_mut` to add and no place walk to
route. That makes it strictly simpler than the array work, and it is the
largest remaining item.

Do not reach for the array *header* next. Refcounting it would remove the one
malloc a copy still makes, but the header is what every holder of a handle
points at, so making it unique means finding and updating those holders — the
design that was deliberately avoided. Measure whether the header shows up at
all after the enum work.

**The VM is a different problem.** Its 2.4 s frame is dominated by the
callback-state value tree: `NativeStateValue::clone` and the `to_vec`/`Vec`
traffic under `from_native_state`/`into_native_state` are most of the sampled
run, with array and struct copying under 10%. The native half stopped using
that tree in `6d07f52` by giving `nativeState` a box in the module's own
layout; the VM half kept it, because state a `@Runtime` function created lives
on the VM's heap and has no native layout. Read the two reverted attempts under
"What did not work" before designing a third — this is the subsystem they both
died in.

The corpus rebuilding its whole view tree every frame (`viewWithOpacity` in
`kira_ui/app/WidgetModel.kira` takes an owned view and rebuilds it) is still
outstanding, and is an algorithm question rather than a compiler one.

## How to measure

Build the editor, then take the **slope** between two frame counts:

```
cd ~/Code/kira-projects/project-matter/apps/editor
kira build
/usr/bin/time -p env KIRA_METAL_OFFSCREEN=1 KIRA_METAL_OFFSCREEN_FRAMES=5  ./app/.kira-build/main
/usr/bin/time -p env KIRA_METAL_OFFSCREEN=1 KIRA_METAL_OFFSCREEN_FRAMES=20 ./app/.kira-build/main
```

`(user20 - user5) / 15` is the per-frame cost. Take the slope, never a single
total: the first run after a build is cold and reads far slower than a warm
one, which produced two wrong conclusions before the method changed. A build
takes 4–8 minutes, so each measurement cycle is ~10 minutes. Now that a frame
is tens of milliseconds, use far more frames — 20 against 220 — or the slope is
mostly startup noise.

The `kira` that builds the editor is `target/debug/kira` in this checkout,
not anything on `PATH`: the runtime archive a program links is the one sitting
beside the compiler that built it, so a repo `kira` and its own archive always
match. `~/.kira/toolchains/dev/1.7.4` is a stale install that predates
`727ae1d` and cannot build the editor at all.

**Compare debug against debug.** A debug archive carries the standard library's
`precondition_check` calls, and they are visible in a profile. Release is worth
about a quarter of a frame here (43 ms → 33 ms) and is the number a user gets,
but every historical figure above it was taken on a debug build.

Profile with `sample <pid> 10 -f /tmp/p.txt` a few seconds into a long run
(`KIRA_METAL_OFFSCREEN_FRAMES=200`), then read the "Sort by top of stack"
block. To attribute a runtime helper to its callers:

```
grep -B 1 "kira_rt_array_clone  (in main)" /tmp/p.txt \
  | grep -oE "kira\.elem\.clone\.[0-9]+|kira_fn_[0-9]+_[A-Za-z_0-9]+" \
  | sort | uniq -c | sort -rn | head
```

A `kira.elem.clone.N` caller means a nested clone: an array of structs whose
fields hold arrays, cloning recursively.

## Proving it renders

Screen capture is denied to this host — `screencapture` fails, and
`CGWindowListCreateImage` was removed in macOS 15. The app reads back its own
drawable instead:

```
KIRA_METAL_ONSCREEN_DUMP=/tmp/f.bgra ./app/.kira-build/main
swift .codex/tmp/bgra2png.swift /tmp/f.bgra 2816 1764 /tmp/f.png
```

That writes the raw BGRA bytes of the *presented* drawable each frame, and the
Swift script converts them and prints a colour histogram — a flat histogram is
a black window. `KIRA_METAL_OFFSCREEN=1` with `KIRA_METAL_IMAGE_DUMP` does the
same for the offscreen path, which proves the renderer but says nothing about
the window; the two differ in exactly the ways that produce a black window with
every live marker still firing. Dimensions are retina: the 1408×882 window
dumps 2816×1764.

Live markers (`live.first_frame` and friends) are **not** proof of rendering. A
black window emits all of them.

## What was fixed, and what each was worth

Nine landed changes, in order. Per-frame cost after each:

1. **`f1fff55` Read an array element without copying the array.** `xs[i]`
   cloned the whole array to read one element, so a loop over `n` elements was
   O(n²). 200,000 reads: 7 s → 0.01 s native, 3m03s → 0.08 s on the VM. This is
   what made an 18 MB mesh load finish at all.
2. **`3eb0961` Keep a borrow a borrow when it is given a second name.**
   `var out = nodes` on a `borrow mut` parameter bound a *copy*, so the layout
   pass appended every node into something nobody could see and the array came
   back empty — the `array index is out of bounds` trap. All three engines did
   it identically, so parity tests could not see it; the oracle prints `2 2 10`
   where kira-rusty printed `0 0`. Fixed above the IR in
   `crates/kira-ir/src/borrow_alias.rs`.
3. **`6d07f52` Hold native callback state in a box instead of a value tree.**
   **21.9 → 5.8 s/frame.** See below.
4. **`c656a0e` Keep a payload-less enum variant in its own handle.**
   **5.8 → 2.9 s/frame.** See below.
5. **`639d922` Read one field of an array element without copying the
   element.** Neutral on this workload, correct in principle, pinned by a
   parity test.
6. **`af867c6` Index an array where it lives instead of copying it out.**
   `tree.nodes[i]` duplicated the whole array because the base is a *field*, not
   a local. Generalized the borrow from "a local" to any addressable place.
   **2.9 → 1.6 s/frame, and startup ~35 s → ~0.1 s.**
7. **`488e92e` Lend a read-only borrow instead of copying it.**
   **1.6 → 1.3 s/frame.** See below.
8. **`0213fe5` Share an array's storage until somebody writes to it.**
   **1.06 → 0.043 s/frame**, measured as a slope on the same build both sides.
   See below.
9. **`182e22d` Share a VM array's elements until somebody writes to them.** The
   same design on the interpreter's heap. Not measurably where a VM frame goes;
   see "What to do next".

### An array copy is a share now

Reading an array copied all of it — every element, and every string, array and
enum inside every element, cloned and then dropped again. A frame is mostly
such reads.

A copy takes a *share* of the item block instead. Each handle keeps a header of
its own, the block behind it carries a count in front of its elements, and the
two mutating entry points — `kira_rt_array_slot_mut`, new, and
`kira_rt_array_push_slot` — give a handle a block of its own before anything
lands in it. The arrays are indistinguishable from two deep copies, at the cost
of one 24-byte header rather than the whole array.

**Sharing the block rather than the header is the whole reason this was
tractable.** Making a block unique moves `items`, a field of the writer's own
header, so no other holder of a handle has to be found and updated and every
header address stays stable — which `xs.append(v)` through a `borrow mut`
parameter depends on. The earlier plan in this document was to refcount the
handle, which would have needed the write path to reach the *holder* of the
handle; that is not necessary and was not done.

Every write goes through the mutable slot: a store, an append, and each `Index`
step of a place walk, so `rows[i].cells[j] = v` unshares both arrays it passes
through. Encoding a value tree moves elements out of a block, which is the same
kind of write, so `kira_rt_native_value_array_from` takes the element clone too.
Filling a block nobody else has seen keeps the plain read slot, and says so at
each of the three places that do it.

`RUNTIME_ABI_VERSION` is **4** and the marker symbol moved with it: an array's
representation changed, and a stale archive would otherwise call two-argument
`push_slot` through a three-argument signature.

The language moves out of a place rather than copying it — `var b = a` on an
array is a move, and the oracle rejects the program that reads `a` afterwards —
so the copies that reach the runtime come from a struct carrying an array
field, a `borrow` read, a return value, and an element read of an array of
structs. Those are the four shapes
`crates/kira-cli/tests/backend_parity/array_sharing.rs` pins, against the
oracle rather than against the three backends agreeing with each other.

### Callback state is a box now

`nativeState` used to encode a Kira value into a backend-neutral tree;
`nativeRecover` cloned the whole tree; reading a field walked it and copied a
subtree; writing a field re-encoded the value. The UI compositor recovers its
batch state once per quad, so drawing one rectangle cost its entire glyph
cache.

A whole-program native module owns the layout of every value it compiles, so it
needs none of that. `nativeState` allocates a box holding the value in that
layout (`crates/kira-native-bridge/src/state_box.rs`), `nativeRecover` is the
box's address, a field read is a load and a field write is a store. The box
carries the type it was made for — checked on every recovery — and the per-type
leaf that drops what its fields own, the same idiom an array already uses for
elements.

The **hybrid half keeps the value tree**, because state a `@Runtime` function
created lives on the VM's heap and has no native layout. One bit of the token
separates them: the store hands out even tokens, a box's address is odd with
its low bit set, so nothing has to look a token up to know which kind it is.
`ModuleKind::HybridLibrary` is the whole of that distinction and
`FunctionLowering::state_is_boxed` is where it is read.

This was the third attempt at the same subsystem. The first two are recorded
under "What did not work".

### A payload-less enum is its own handle

Every enum value was a heap box, so an axis, an alignment, or a sizing mode —
a variant that is nothing but a tag — cost a `malloc` to construct and another
on every read that cloned it. A layout descriptor is mostly such variants.

A tag fits in the handle, so it lives there as `(tag << 1) | 1`. The backend
emits a constant, no call and no allocation; the runtime reads the low bit, and
cloning one is identity while freeing one is nothing. A box comes from the
allocator word-aligned, so that bit is free.

This changed what a `KEnum` is at the native ABI, so `RUNTIME_ABI_VERSION` is
**3** and the marker symbol moved with it. A stale archive fails the link by
name rather than reading a tag out of a pointer.

### A read-only borrow is lent, not copied

`borrow` says the caller keeps the value, so there is nothing for the callee to
own and nothing to copy — but every such parameter was passed by value. A view
tree recursing over its children copied each child's *entire subtree*, and the
layout tree passed down beside it copied every node and descriptor at every
level.

A `borrow` parameter of a type worth copying now arrives as a pointer
(`IrFunction::by_pointer_params`, computed in `crates/kira-ir/src/lower.rs`).
When the argument names a place its address goes over; anything else is
evaluated into a temporary the caller owns, lends, and drops after the call.

Only `ModuleKind::Executable` does this. Every caller has to agree with the
signature, and that is the one shape where the module compiles them all — a
hybrid half is called by the VM through a trampoline, a library by a consumer
through its export surface, a sidecar by a host.

One correctness catch fell out: **rebinding a read-only borrow and writing
through the new name goes back to copying.** It has to. `borrow` lends no
permission to write, and with real pointers the write would otherwise land in
the caller's value. `borrow mut` is exactly that permission, so it still
aliases. Pinned by `a_lent_borrow_is_still_read_only_to_the_callee`.

## What did not work

Two attempts at the native-state cost, both implemented, measured, and
reverted. Read their commit messages before proposing a third.

**Copy-on-write children** (`ccff87c`, reverted `db2e598`). Putting a struct's
and array's children behind an `Arc` so a clone is a refcount bump made the
frame **65% worse**: 21.9 → 36.2 s/frame. Writes go one field at a time through
a shared tree, so `Arc::make_mut` copied on every write instead of once per
recover.

**A read-only view node** (`ed25e0f`, reverted `87fd1d7`). A shared root plus
the path walked into it, so reads copy nothing. Reads did get cheap — the
profile moved off `NativeStateValue::clone` entirely onto `walk` — but 20
frames went from ~9 minutes to over 55. Rebuilding the state to replace it then
clones a subtree per nesting level, which costs more than the one eager copy it
removed.

The lesson that produced the box: reads and writes have to move together.
Tuning the representation for one at the other's expense lost twice.

Neither of those is an argument against the array sharing that later worked,
and the difference is worth being precise about. Both failures shared a *tree*
that was rebuilt field by field, so every write paid a copy. An array's block is
shared and made unique once, by the first write through a handle, and every
write after that is a count and a compare. What loses is deferring a copy to a
path that runs more often than the one it was deferred from.

Also reverted earlier in the session, worth not repeating:

- **Defaulting development builds to unoptimized.** Without optimization LLVM
  does not colour stack slots, so the editor's widget dispatch reserved 336 KB
  per frame and overflowed 34 frames deep. There is no unoptimized level any
  more.
- **Hoisting allocas to the entry block** to fix a foreign-call stack leak. It
  fixed the leak and caused the same overflow. `llvm.stacksave` /
  `llvm.stackrestore` around each call site is the version that works
  (`7eedb90`).

## Other things fixed along the way

**`kira_ui` trapped on the only platform that uses it.** The UI compositor
called `kira_ui_write_quad` for every rectangle; no toolchain ships that
library, it was declared `Availability.Optional`, so the call was excluded from
the link and trapped by name. The compositor writes its own 23 floats per
vertex now, in Kira, through the runtime's raw-memory helpers
(`ui-foundation/app/Backend/UiBatchForeign.kira`). The vertex layout is
specified by the corpus's own MSL `UiVertex` struct in `UiBatch.kira`.

**MSL per-stage uniform binding.** Vertex uniforms bind at `slot + 1`, fragment
at `slot`. The host's own comment documents it.

**`kira run --backend vm` could not run a program with a foreign half**
(`727ae1d`). Two refusals. The adapter sidecar generated an adapter for every
foreign import including ones whose library the target lacks, so the *link*
failed naming Vulkan and Direct3D entry points on a Mac — the whole-program
native build already answered those with a status, and the sidecar and hybrid
half never got the same list. Then the host a VM run installs when the program
has a foreign half implemented every capability *except* the filesystem, so it
fell through to a refusal in exactly the programs most likely to open a file.
The editor now reaches `live.first_frame` on the VM engine as on the native
one.

**Array traps print a backtrace on demand.** The message cannot name the index
or length without diverging from the wasm trap path, so the detail goes out of
band through `KIRA_TRAP_BACKTRACE=1`. That is what named
`foundationLayoutAppendDescendants` in one run and made the borrow-alias bug
findable.

## Corpus commits

Two sibling repos carry changes:

- `kira-graphics` — `82e0fa7` adds `KIRA_METAL_ONSCREEN_DUMP` and drops the
  `kira_ui` native library declaration; `abde71c` reports a Metal shader that
  would not compile instead of swallowing the NSError.
- `ui-foundation` — `7eb3109` writes a UI quad's vertices in Kira.

## Ground rules that cost time to learn

Gates are `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo nextest run --workspace` (2193 tests). Run all three before every
commit; the `verifying-work` skill has the full bar.

The offscreen run prints a checksum of the frame it produced. Two builds
printing the same one is the cheapest evidence there is that a representation
change is invisible — it is what confirmed the array sharing, on native and on
the VM.

Pin new behaviour against **the oracle**
(`~/.kira/toolchains/dev/1.7.3/bin/kira`), not against the three backends
agreeing with each other. Every engine copying a borrow identically is exactly
the bug that produced the layout trap, and `assert_parity` could not see it
because all three agreed. `assert_parity` returns the output, so assert the
value.

The oracle rejects some programs kira-rusty accepts — a chain of rebound
borrows is "overlapping place access" there. Do not pin behaviour it refuses.

macOS has no `timeout`. Bound a command with
`perl -e 'alarm shift; exec @ARGV or die "exec: $!"' 60 <command>`.

The editor's real artifact is `app/.kira-build/main`. `.kira-build/editor` at
the package root is a stale bootstrapper output and misled one investigation.
