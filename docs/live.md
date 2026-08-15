# Kira Live

`kira live [file|dir]` builds a program into a `.klbundle`, serves it over a
loopback socket, and starts a runner client that downloads it, loads it, links
it, and starts it. With `--watch`, every save rebuilds and the change goes into
the app that is already running.

It is a server and a client, not a rebuild loop. The app is hosted somewhere
that outlives the compiler and can take a new bundle later — which is the whole
reason reload is possible at all.

An unwatched session lasts as long as the app does. A program that prints and
returns ends it in milliseconds; an app ends it when its window closes. The two
are the same wait, which is what makes `kira live` a way to run an app and not
only a way to start one.

```sh
kira live                                      # the package you are standing in
kira live app.kira                             # the VM backend
kira live --backend llvm app.kira              # the whole native program
kira live --backend hybrid app.kira            # the VM/native hybrid
kira live --watch app.kira                     # reload on every save
kira live --watch --quit-after 30s app.kira    # bounded, for scripts and CI
kira live ios app.kira                         # a runner with no client yet: says so
```

A path is optional, and naming none means the package you are standing in — the
same default `run`, `build`, and `check` take. The path goes through the same
package discovery either way, so a directory holding no `package.kira` is
refused by name.

A program that calls C gets a generated adapter sidecar on the VM backend.
`@FFI.Extern` reaches a C symbol through that sidecar, while the bytecode
payload remains the bundle entrypoint. The LLVM backend compiles the whole
program into one native library with a fixed runner entry symbol and stages its
foreign archives and runtime files beside it. The hybrid backend instead carries
a hybrid manifest, bytecode, and native library.

| Flag | Meaning |
|---|---|
| `--backend vm\|llvm\|hybrid` | the VM, whole-native, or hybrid bundle shape; `vm` by default |
| `--watch` | rebuild and reload on every save |
| `--quit-after <5s\|500ms\|2m>` | shut the session down cleanly after this long |

The first positional is a runner id if it names one and a path otherwise, so
`kira live ios` is the iOS runner and `kira live ./ios` is a directory. The
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
    libapp_ffi.dylib
    libapp.dylib
    libapp_live.dylib    whole-program LLVM live entry
    libffifixture.a      native link asset, when selected
    runtime.dll          native runtime asset, when selected
```

The manifest records the runner and profile the bundle was built for, one row per
payload (name, kind, SHA-256, size), and which payload is the entrypoint.
Payloads are staged flat because a `KHM1` hybrid manifest names its bytecode and
library as file names beside itself, and a whole-native library resolves its
bundled runtime files from the same directory. Staging them as siblings is what
makes both forms resolve inside a runner's cache exactly as they did in the
build directory.

Bundles arrive over a socket, so the format is a validated wire format rather
than a struct that happens to serialize: every truncation, unknown tag, and
out-of-range index is a typed error, and payload names are checked to be plain
file names once, at the decoder, because they become paths on disk. Payloads are
verified against the manifest on arrival — a runner holds the bytes the build
produced, or it holds an error.

Payload hashes are SHA-256 identity fingerprints. A collision could make a
changed payload look unchanged, so reload never treats a changed bytecode
payload as compatible without separate live-value evidence.

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
live.session.ready       live.app.exited             [reason=…]
live.watch.started       live.shutdown.started / live.shutdown.finished
```

The four reload events are deliberately distinct: `notified` is the supervisor
asking, `staged` is loaded-but-not-live, `applied` is committed, and `completed`
means the swapped-in code has run once without incident. A swap that commits and
then traps on its first call is not a reload that worked.

`live.app.exited` is the app's entrypoint returning, which is not the runner
ending. The runner outlives it, holding the cache and the loaded library that
make the next reload cheap. An unwatched session ends there because it has
nothing else to do; a watched one reports it and keeps watching, and the next
save starts the app again.

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

Reload decisions use the `KLB1` manifest. Each payload row carries a name, kind,
SHA-256 hash, and size. An exact manifest match is `Unchanged`.

**Hot patch**

A hot patch requires an in-process replacement operation and compatibility
evidence for live values. The current `KLB1` manifest has payload identity only.
It has no struct or enum layout or closure signature fingerprint, so
`reload::decide` relaunches every changed bytecode payload, including a hybrid
bundle whose native library and hybrid manifest are unchanged. The runner can
also refuse a proposed swap when its app thread or live values cannot accept it.

Matching hash and size prove payload identity. They do not prove that a different
bytecode artifact is safe beside live values.

**Relaunch**

The runner is replaced, and the reason is reported. The rule is manifest
evidence, not a source diff. A source edit that produces an identical manifest
does nothing. A source edit that changes bytecode, native code, the hybrid
manifest, or an asset relaunches.

| Relaunch reason | When |
|---|---|
| the native library changed | its bytes moved; the mapped code is stale |
| the hybrid manifest changed | the VM/native boundary moved |
| the bytecode changed | `KLB1` has no layout or closure compatibility evidence |
| a payload changed | something not swappable in place, e.g. an asset |
| the bundle's payloads changed | one appeared, vanished, was renamed, or changed kind |
| the entrypoint moved / is not swappable | a different program shape |
| the bundle is for a different runner or profile | not the same app |
| hot patching is disabled | `KIRA_LIVE_NO_HOTPATCH=1` |
| the app's entrypoint is still running | a run loop has a call stack in the code the swap would replace |
| the runner refused | only it knows what its live values depend on |

Unsafe or rejected swaps report their reason. Direct relaunch decisions and
runner refusals are both visible to the session.

`KIRA_LIVE_NO_HOTPATCH=1` turns tier 1 off entirely. The runner refuses every
swap and every reload relaunches.

A save that changes nothing does nothing. A save that does not compile prints its
diagnostics and leaves the running app alone: killing a working app over a
half-typed line would make watching worse than not watching.

A running app's entrypoint stays on its run loop, so the runner refuses a swap
until it has a safe frame boundary.

### What survives

A compatible hot patch keeps the current process, loaded native libraries, and
values owned by them. A relaunch loses process state.

The current `KLB1` manifest records payload identity only. It does not record
struct or enum layouts or closure function signatures. `reload::decide` treats
every changed bytecode payload as unsafe and returns `Relaunch` until that
compatibility evidence exists.

## Watching

The watch set is the path the invocation named: one file for a standalone
program, and the whole directory for a package, so a save anywhere under `app/`
reloads rather than only a save to the entry.

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

`kira-desktop-runner` is the client that ships today. It hosts VM bytecode, a
whole-program native library, and a hybrid entrypoint. It loads the LLVM live
library and calls its fixed entry symbol in the runner process. Running a bundle
needs no LLVM, only building one does.

**The app gets the main thread; the protocol gets another.** A Kira app is not a
function that returns, and a runner that started one on the thread holding the
socket would never hear another word from the server, so `entrypoint started`
could only ever be reported by an app that had already exited, which is to say
never by an app. So the app keeps the main thread, which is also what macOS
requires of a window's run loop, and the protocol runs beside it. Load, link,
start, and swap all still happen on that one thread, in order: the protocol
thread asks and the app's thread answers. The only call whose meaning changes is
`start`, which is answered when the entrypoint is *running*, and answered a
second time, for the reload path, when it returns.

Every runner id parses. One this build has no client for (`ios`, `android`, and
the rest) reports precisely that rather than failing as an unknown command: the
runner is modeled, the command is valid, and the diagnostic names what is
missing.

The runner is resolved beside `kira` rather than from `PATH`, so a session never
picks up a runner from a different build than the bundle it is about to serve.
An installed toolchain therefore ships one in its `bin/`, staged by `knvm
binstall` and packaged by the release workflow alongside the compiler and the
language server. In a checkout it is `cargo build --workspace` that puts one in
`target/debug`: nothing depends on the runner, and cargo builds a dependency's
lib target and never its `[[bin]]`, so `cargo build -p kira-cli` leaves none. A
session that cannot find its runner says so, and names both routes.

## Limits

- **A session's own bar is the entrypoint.** It stops at
  `live.entrypoint.started` and never claims `live.frame.presented`. An app
  hosted here does present frames — it brings its own window and swapchain from
  kira-graphics — but the milestone belongs to the end that can observe it, and
  this repo owns neither.
- **The session socket is unauthenticated.** It is loopback and first-come, so
  any local process that wins the accept gets the bundle.
- **`--quit-after` bounds the session, not a rebuild.** A save landing near the
  deadline can overshoot it by however long the rebuild and reload take.
- **Every wait is bounded except one.** Reads, writes, and the accept each have a
  30-second timeout; waiting for the next save is deliberately unbounded, because
  a runner that killed itself for the crime of a developer thinking is a runner
  whose app is gone by the time they save. A dead server closes the socket, and a
  closed socket is a read of zero bytes rather than a hang.
