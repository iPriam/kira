# Kira Live

`kirac live <file>` builds a program into a `.klbundle`, serves it over a
loopback socket, and starts a runner client that downloads it, loads it, links
it, and starts it. With `--watch`, every save rebuilds and the change goes into
the app that is already running.

It is a server and a client, not a rebuild loop. The app is hosted somewhere
that outlives the compiler and can take a new bundle later — which is the whole
reason reload is possible at all.

```sh
kirac live app.kira                             # the VM half
kirac live --backend hybrid app.kira            # both halves
kirac live --watch app.kira                     # reload on every save
kirac live --watch --quit-after 30s app.kira    # bounded, for scripts and CI
kirac live ios app.kira                         # a runner with no client yet: says so
```

| Flag | Meaning |
|---|---|
| `--backend vm\|hybrid` | which halves the bundle carries; `vm` by default |
| `--watch` | rebuild and reload on every save |
| `--quit-after <5s\|500ms\|2m>` | shut the session down cleanly after this long |

The first positional is a runner id if it names one and a path otherwise, so
`kirac live ios` is the iOS runner and `kirac live ./ios` is a directory. The
distinction is made on shape, not on what happens to exist: a path-looking
argument stays a path even when nothing is there, so the error says the file is
missing rather than that the runner is unknown.

## The bundle is the boundary

A runner reads a `.klbundle` and nothing else — never a compiler data structure,
never a path into a build tree. That is what lets the compiler's internals change
without breaking every runner, and it is why `kira-desktop-runner` has no
dependency on `kira-ir`, on any backend, or on LLVM.

```text
app.klbundle/
  manifest.klb          the KLB1 manifest
  payloads/
    app.khm             one file per payload, named by the manifest
    app.kbc
    libapp.dylib
```

The manifest records the runner and profile the bundle was built for, one row per
payload (name, kind, SHA-256, size), and which payload is the entrypoint.
Payloads are staged flat because a `KHM1` hybrid manifest names its bytecode and
library as file names beside itself — staging them as siblings is what makes it
resolve inside a runner's cache exactly as it did in the build directory.

Bundles arrive over a socket, so the format is a validated wire format rather
than a struct that happens to serialize: every truncation, unknown tag, and
out-of-range index is a typed error, and payload names are checked to be plain
file names once, at the decoder, because they become paths on disk. Payloads are
verified against the manifest on arrival — a runner holds the bytes the build
produced, or it holds an error.

The hash is SHA-256 rather than a checksum because reload decides from it. A
collision would hot-patch across an ABI change and corrupt memory silently.

## Events

A session reports the `live.*` vocabulary, and each event is emitted where its
milestone actually happened. `live.reload.completed` goes out after the new code
*ran*, not after it was sent in the hope that it would.

```text
live.server.started      live.source.changed
live.bundle.built        live.bundle.rebuilt
live.client.connected    live.reload.notified        mode=hotpatch|relaunch [reason=…]
live.bundle.requested    live.reload.staged
live.bundle.sent         live.reload.applied
live.bundle.received     live.reload.completed       mode=hotpatch
live.bundle.loaded       live.reload.rejected        reason=…
live.bundle.linked       live.reload.restart_required reason=…
live.entrypoint.started  live.runner.relaunched
live.session.ready       live.shutdown.started / live.shutdown.finished
```

The four reload events are deliberately distinct: `notified` is the supervisor
asking, `staged` is loaded-but-not-live, `applied` is committed, and `completed`
means the swapped-in code has run once without incident. A swap that commits and
then traps on its first call is not a reload that worked.

**Each milestone belongs to the end that can know it.** The server observes that
a runner connected and that bytes went out; only the runner can report that they
loaded. A runner that reports one of the server's own milestones is refused —
otherwise it could assert its way to a ready session with no bundle ever served.
A session is ready only once every required milestone has arrived, in order.

Be precise about what that buys. The server enforces *ordering and ownership*,
not honesty: a runner that downloads a bundle, discards it, and reports each
milestone in order will be believed, because the server cannot see inside it.
That is not a hole — it is where the trust boundary sits. The runner is the thing
being trusted to run the app, so the evidence a session is real comes from the
app's own behavior, which is why the end-to-end tests assert on its stdout rather
than on milestones.

## Reload

Two tiers, chosen by what actually changed.

**Hot patch** — the rebuilt native library is byte-for-byte the loaded one, so
the edit was a bytecode-only edit whatever the source looked like. The bytecode
swaps into the running process: same process, same mapped library, nothing
re-`dlopen`ed. Only payloads whose hash moved are rewritten, so the byte-identical
library is exactly the file left untouched, and the replacement is built before
the old one is dropped — the library's refcount never reaches zero, so the loader
never unmaps it and its addresses stay put.

**Relaunch** — anything else. The runner is replaced, and the reason is reported.

The rule is byte identity, not a source diff. A `@Runtime` edit in a hybrid app
rebuilds the library identically and hot-patches; a `@Native` edit does not and
cannot, because the process has that library's code mapped and native state
holding pointers into it.

| Relaunch reason | When |
|---|---|
| the native library changed | its bytes moved; the mapped code is stale |
| the hybrid manifest changed | the VM/native boundary moved |
| a payload changed | something not swappable in place, e.g. an asset |
| the bundle's payloads changed | one appeared, vanished, was renamed, or changed kind |
| the entrypoint moved / is not swappable | a different program shape |
| the bundle is for a different runner or profile | not the same app |
| hot patching is disabled | `KIRA_LIVE_NO_HOTPATCH=1` |
| the runner refused | only it knows what its live values depend on |

Nothing degrades quietly. A bundle that cannot be hot-patched says so and says
why, rather than relaunching silently and leaving someone wondering where their
state went. The supervisor always attempts tier 1 first and announces the
fallback, so a relaunch never looks like a slow hot patch.

`KIRA_LIVE_NO_HOTPATCH=1` turns tier 1 off entirely — the runner refuses every
swap and every reload relaunches. It exists so a session can run with the
hot-patch path *removed* rather than merely unused, which is what makes it
possible to tell whether a bug belongs to it.

A save that changes nothing does nothing. A save that does not compile prints its
diagnostics and leaves the running app alone: killing a working app over a
half-typed line would make watching worse than not watching.

### What survives

Today: the process and its loaded native library.

*App state* surviving is the eventual promise, and it is not testable yet — the
language has no globals and no closures, so nothing outlives a call. The two
rejection conditions that protect such state, a struct or enum whose layout
changed and a live closure whose function changed signature, are **not checked**,
because neither can happen yet. They are not skipped; there is nothing to skip.
`kira-live`'s `reload::decide` is where they land when those features do, and the
hot patch must not be trusted for them until they are there.

## Watching

The interesting part is what is *not* watched. A watcher that sees its own build
output rebuilds forever, and an editor writing `app.kira~` and `.app.kira.swp` on
the way to saving would trigger three rebuilds per save, two of them of an
unchanged program.

| Ignored | |
|---|---|
| directories | `.kira-build`, `exports`, `zig-out`, `generated`, `target`, and every dot-directory |
| files | dotfiles, and anything ending `~`, `.swp`, `.swx`, `.tmp` |

Both lists are matched on every host rather than the one that produces them: a
session runs on a machine whose editor and toolchain are not knowable in advance.
Dot-directories go wholesale because naming `.git`, `.svn`, and each editor's
private directory one at a time is a list that is always one entry out of date.

The watcher polls every 150ms and compares modification time and size. Polling is
unglamorous and portable, and a burst of saves during a build collapses into one
rebuild rather than queueing three.

## Runners

`kira-desktop-runner` is the client that ships today. It hosts a VM bytecode
entrypoint and a hybrid one, `dlopen`ing the native half for the latter. Running
a bundle needs no LLVM — only building one does — which is what lets the native
path be real rather than deferred.

Every runner id parses. One this build has no client for (`ios`, `android`, and
the rest) reports precisely that rather than failing as an unknown command: the
runner is modeled, the command is valid, and the diagnostic names what is
missing.

The runner is resolved beside `kirac` rather than from `PATH`, so a session never
picks up a runner from a different build than the bundle it is about to serve.
Cargo builds a dependency's lib target and never its `[[bin]]`, so it is
`cargo build --workspace` that puts it there — `cargo build -p kira-cli` does not,
and a session that cannot find its runner says so.

## Limits

- **Headless.** Sessions stop at `live.entrypoint.started` and never claim
  `live.frame.presented`. Presenting a frame needs a window and a swapchain, and
  kira-graphics owns those, not this repo.
- **One file is the watch set.** That is what `kirac live` is given; there are no
  packages, manifests, or `app/` directory to walk yet. `WatchSet` takes roots
  precisely so this grows without the watching changing.
- **The session socket is unauthenticated.** It is loopback and first-come, so
  any local process that wins the accept gets the bundle.
- **`--quit-after` bounds the session, not a rebuild.** A save landing near the
  deadline can overshoot it by however long the rebuild and reload take.
- **Every wait is bounded except one.** Reads, writes, and the accept each have a
  30-second timeout; waiting for the next save is deliberately unbounded, because
  a runner that killed itself for the crime of a developer thinking is a runner
  whose app is gone by the time they save. A dead server closes the socket, and a
  closed socket is a read of zero bytes rather than a hang.
