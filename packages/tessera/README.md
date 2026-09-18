# Tessera

LibTessera, the system interface of TesseraOS, written in Kira. `import Tessera`
and a path dependency:

```kira
let dependencies = [
    Dependency { name: "Tessera", path: "../kira/packages/tessera" }
]
```

Everything a process can name is a handle: an unforgeable, typed, process-local
`U64` carrying rights. What a process may do is what its handles allow. Any
operation that leaves the process is submitted with a `(port, key)` and produces
one completion; a port is the only place results arrive. No call, struct, flag or
error names a kernel concept, so the surface is identical whatever runs beneath
it.

The full specification is the LibTessera System Interface Specification. This
package is its portable front: the kernel-independent half that the spec (§21)
separates from the per-kernel backends. It runs on every Kira backend (`vm`,
`llvm`, `hybrid`, `wasm`) because it is pure library state, not a kernel binding.

## What is here

- Boundary types (`Types.kira`): `Handle`, `Key`, `Rights`, `Signals`,
  `Instant`, `Duration`, `Deadline`, `ByteSize`, `Address`, `Buffer`,
  `ObjectType`. Each width is a `distinct` type, so a `Handle` cannot be passed
  where a `Key` is expected. The spec's `Size` is `ByteSize` here, because
  Foundation's geometry owns the bare name.
- Rights (`Rights.kira`): the generic and type-specific right bits (§3.3) and the
  set algebra that duplicate and replace narrow authority through.
- Signals (`Signals.kira`): the per-type and user signal bits (§3.4).
- Errors (`Errors.kira`): the `Error` value, `ErrorCode`, `Subsystem`, and the
  wire-number mappings (§4).
- Ports and completions (`Completion.kira`): the `Completion` packet, its kinds,
  and `WaitOptions` (§5).
- The runtime (`Runtime.kira`): the per-process handle table and object store,
  handle-value generation checks, and the signal-to-completion delivery path.
- The generic calls (`Object.kira`): `handleClose`, `handleCloseMany`,
  `handleDuplicate`, `handleReplace`, `handleInfo`, `objectSetName`,
  `objectWaitAsync`, `objectSignal` (§3.5).
- Ports (`Port.kira`): `portCreate`, `portWait`, `portPost`, `portCancel` (§5).
- Events (`Event.kira`): `eventCreate` and `eventPairCreate` (§9), the first two
  concrete object types, chosen because they need no kernel and so exercise the
  whole handle, rights, signal and completion machinery on every backend.

## The process is a value

The specification's calls are free functions over an implicit per-process handle
table, which is what the C ABI mints from a process-global. This package makes
that state a `Tessera` value and threads it explicitly: `handleClose(process, h)`
rather than `Handle.close(h)`. A kernel backend keeps the same call shapes over
the same state, so the front does not change when a backend is added.

## Why the C export layer is not here yet

The specification exports every declaration to C as `@Export("tessera_…")` with
fixed struct layouts. Kira's `@Export` today is a bare marker on functions and
refuses structs, so the named-symbol and struct-layout ABI is compiler work that
lands with the export layer. The Kira model in this package is the source of
truth that layer will export; building it first is what lets the ABI be
mechanical rather than invented.

## What is not here yet

The spec's other domains build on this core: Memory (§6), Threads and
synchronisation (§7), Time and timers (§8), Channels (§10), Processes and jobs
(§11), the System channel protocol (§12), Namespace and files (§13), Grants and
pickers (§14), Network (§15), Device streams (§16), GPU and fences (§17), Power
(§18), Random (§19) and Diagnostics (§20). Each is added as its own module over
this same runtime, with its object types, calls, rights and errors, and its
per-kernel backend.
