# tessera-harness

The `Test` suite for `packages/tessera`, the LibTessera portable front. Every
Kira behaviour the package exposes is pinned here.

The front is kernel-independent library state, so it runs on every backend:

```sh
kira test --backend vm     tests-kik/tessera-harness
kira test --backend llvm   tests-kik/tessera-harness
kira test --backend hybrid tests-kik/tessera-harness
```

Each test builds a fresh `Tessera` process, drives real calls, and reduces the
outcome to an `Int` the `expect` compares. The suite covers the handle table's
rights and generation checks, the signal-to-completion delivery path,
`waitAsync`'s Once, Repeat and Edge arming, port draining and cancellation, and
the EventPair peer-death and cross-end signalling. Failure cases assert the exact
`ErrorCode`: a missing right is `AccessDenied`, a stale handle is `InvalidHandle`,
signalling a Port is `WrongType`.

All top-level names use the `tsx`/`Tsx` prefix, so the harness shares a package
namespace with the copied `Test` and `TestRunner` infrastructure without
collision.
