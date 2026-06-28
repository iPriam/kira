# FFI harness findings

Bridge bugs surfaced while building the FFI / native-bridge `Test` suite.

## FF1. Captured-scalar `(Int) -> Int` closures collide on function_id across the bridge (OPEN)

When several VM closures that each capture a scalar `let` are passed across the
hybrid bridge to `@Native` and invoked, they all dispatch to the SAME callback —
the first captured closure in compilation order — instead of their own bodies.
The hybrid closure dispatcher's switch-table keys captured-scalar closures by a
shared `function_id` rather than per-closure identity, so `invoke(move cbA)` and
`invoke(move cbB)` both run `cbA`.

Surfaced authoring `app/closures/FcbCallbackTests.kira`. Non-capturing closures
and a single captured closure work; only multiple distinct captured-scalar
closures misdispatch. 12 capture-based tests were excluded (listed in that file's
header) until the dispatcher keys closures by identity. Likely in the hybrid
native-bridge closure thunk/`function_id` assignment for captured closures
(`materializeNativeClosure` / the callback dispatch path).

Minimal shape:
```
@Native function inv(cb: (Int) -> Int, x: Int) -> Int { return cb(x) }
// in a test: let a = 10; let cbA = { v in return v + a }
//            let b = 100; let cbB = { v in return v + b }
//            inv(move cbA, 1) + inv(move cbB, 1)   // expect 112; bug gives 22 (cbA twice)
```
