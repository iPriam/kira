# FFI harness findings

The callback harness records the supported bridge behavior and its limits.

## FF1. Distinct captured closures

Group K in `app/closures/FcbCallbackTests.kira` verifies scalar `let` captures,
distinct captures in one call, sequential callbacks, loops, and native
re-entry. Each closure keeps its own capture and teardown completes.

Function-typed closures use the native bridge. The C `@FFI.Callback` path
accepts named top-level functions only. Method values, aggregate captures, and
aggregate callback results remain unsupported.
