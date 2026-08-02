# knvm

`knvm` installs Kira. It is the first thing a user runs, before any `kira`
exists on the machine:

```sh
knvm install latest
knvm install 1.7.3 --channel dev
knvm install-llvm
knvm binstall
knvm sinstall
knvm self-update
knvm list
knvm list --remote
knvm use 1.7.3
knvm pin 1.7.3
knvm unpin
knvm uninstall 1.7.3
```

`install` fetches a release, unpacks it into
`~/.kira/toolchains/<channel>/<version>/`, validates it, and selects it by
writing `current.toml`. `list`, `use`, `pin`, `unpin`, and `uninstall` operate
on what is already on disk and never reach a network. `--channel` defaults to
`release` everywhere, and `switch` is accepted as a spelling of `use`.

On a machine with nothing installed, the bootstrap comes first:

```sh
curl -fsSL https://kira-lang.com/install.sh | sh   # or install.ps1 on Windows
```

Selection is what the `kira` launcher reads. It resolves `current.toml`, finds
the selected toolchain's primary binary, and hands the process over with every
argument and the whole environment forwarded untouched: on unix by replacing
its own process image, so the exit status, signals, and stdio belong to `kira`
by construction, and on Windows by waiting and forwarding the exit code. Exit
code 2 from `kira` therefore means exactly one thing - the launcher itself
could not dispatch - and each such failure names `knvm install latest` as the
remedy.

The launcher is multi-call: installed under the name `kira-language-server`
(a second copy `sinstall` lands beside `kira`), it dispatches to the selected
toolchain's language server instead of its primary. That is what an editor
finds on PATH, so the server an editor runs always matches the selected
compiler and never goes stale when the language moves.

## The tree it produces

```
~/.kira/toolchains/
 current.toml channel = "release", version = "1.10.0", primary = "kira"
 release/1.10.0/
 bin/kira
 bin/kira-language-server the editor server, same frontend as its kira
 bin/kira-desktop-runner the client `kira live` starts, built by the same kira
 bin/libkira_native_bridge.a the native runtime
 bin/libkira_compiler_bridge.a native runtime plus the compiler capability
 bin/libkira_native_bridge-wasm32-emscripten.a the Web runtime
 foundation/ the standard library, a real Kira package
 package.kira
 app/*.kira
 packages/kira_main
 templates/{app,library}
 llvm/ , libffi/ shared siblings; knvm never touches them
```

`foundation/package.kira` beside `bin/` is what makes the tree usable rather
than merely present: it is the marker `kira-toolchain`'s `bundled_discovery`
looks for, and install refuses an archive that lacks it. `current.toml` is not
inert bookkeeping either - discovery rule 3 reads it to find Foundation for a
consumer that is not `kira` itself, such as a `build.rs` running through
`kira-build`. See [docs/foundation.md](foundation.md).

Every one of those paths comes from `kira-toolchain`. The `kira-knvm` crate
defines no layout of its own, so there is exactly one definition of what an
installed toolchain looks like, shared by the thing that writes it and the
thing that reads it.

## What an install does

Resolving `latest` lists the channel and takes the newest version; an exact
version is fetched directly, costing one transfer instead of a listing plus a
transfer. A version already present under `<channel>/<version>/` skips the
fetch entirely and is selected as it stands - `install` always implies `use`.

Otherwise the archive is fetched into a staging directory belonging to this
process alone, checked against the digest published beside it, unpacked with
the system `tar`, and validated. The tree must
hold executable `bin/kira` and `bin/kira-language-server`, all three runtime
archives shown above, and `foundation/package.kira`. Requiring the language
server prevents an editor from silently finding a stale server elsewhere;
requiring the archives prevents the compiler from being selected when it
cannot run native, compiler-using, or Web programs. Only then is the validated
tree moved into place with a single `rename`, and `current.toml` written.
Nothing appears under `<channel>/` until the toolchain is known-good, so a
failed install leaves no half-toolchain a launcher could dispatch to. Staging
is removed on every exit path.

The one hole in that guarantee is `SIGKILL`: the staging cleanup is a `Drop`
guard, which a hard kill does not run, leaving an inert directory under
`.staging/` that nothing prunes.

## Checksums

Every published archive carries a `<archive>.sha256` sidecar, and every route
that downloads one verifies it before anything is unpacked: `install`,
`install-llvm`, `self-update`, and both bootstrap scripts. A mismatch is fatal
and installs nothing. An artifact with no sidecar still installs — releases cut
before sidecars existed have none — and says so, because "verified" and "no
checksum was published" are different installs and a user who cannot tell them
apart has no way to notice the day verification stops happening.

The digest is computed by `kira-knvm`'s own SHA-256, implemented in the crate
rather than taken from a dependency, for the same reason the transport is a
`curl` subprocess: the workspace's external dependency set is frozen. It is
pinned by the published FIPS 180-4 vectors, the block-boundary padding lengths,
and a test that hashes one message in seven chunkings and requires all seven to
agree.

What this is worth is bounded and worth stating. The transport is HTTPS, so TLS
is what stands between a user and a hostile network; the checksum catches a
truncated transfer, a stale mirror, and an artifact repackaged after
publication. An attacker who can delete the sidecar from a TLS-served release
already owns the archive beside it, which is why an absent sidecar downgrades
rather than refuses.

## Listing, selecting, removing

`list` walks the channel directories, reports `release` before `dev` and newest
first within each, and marks the selected one with `*`. A toolchains root that
does not exist is an empty listing rather than an error - it is the state of a
machine that has never run `knvm install`. A version whose tree has lost its
`bin/kira` is listed and flagged broken instead of being hidden.

`use` refuses a version that is not installed, and equally refuses one missing
the binary the launcher runs: selecting either would leave `kira` unable to
dispatch, which is better reported now than at the next invocation.

`uninstall` removes exactly `<channel>/<version>/`. When the removed version
was the selected one, `current.toml` is deleted and the removal warns that
nothing is selected - repointing at some surviving version would silently
change which compiler a user runs.

`list --remote` asks the same question of the release feed instead of of the
disk: what each channel publishes, newest first. It is a flag on `list` rather
than a verb of its own because it is one question with two sources.

## Pinning a directory tree

`current.toml` is a single global selection, which is the wrong granularity for
a machine building two projects that want different compilers. `knvm pin
1.10.0` writes a `kira-toolchain.toml` in the working directory:

```toml
channel = "release"
version = "1.10.0"
```

The launcher walks up from its working directory, and the nearest pin wins over
the global selection - the same precedence a project's `rust-toolchain.toml`
has over a default toolchain. `knvm unpin` removes it.

`pin` refuses a version that is not installed, because writing one would leave
every later `kira` in that tree unable to dispatch. The launcher refuses a pin
whose toolchain has since been removed, and refuses one it cannot parse,
rather than falling back: falling back would silently run the compiler the
project explicitly said it does not want, which is the one outcome a pin exists
to prevent. Both refusals name the remedy.

A pin says which toolchain, never which binary inside it. Which binary to run
stays the launcher's own decision, made from `argv[0]`, so the multi-call
language-server alias keeps working inside a pinned tree.

## `install-llvm`: the backend's LLVM

The LLVM backend is a hard dependency of every `kira`, and its build script
discovers a bundle at `~/.kira/toolchains/llvm/<version>/<host-key>` without
being told where it is. `knvm install-llvm` is what puts one there; before it,
the only routes were a CI step and a developer running `tar` by hand.

Nothing about the version is decided here. `llvm-metadata.toml` - compiled into
`kira-toolchain` - names the LLVM version, the release tag owning the published
bundles, and the exact asset per host, and this downloads what that says. A
knvm built from a checkout whose pin has moved provisions the new bundle by
construction.

An already-usable bundle is left alone; `--force` removes and refetches it, the
repair route for a tree interrupted mid-extraction. The test for "usable" is
`include/llvm-c/Core.h`, which is exactly the test discovery applies, so
nothing can be installed that discovery would then fail to find. The bundle
lands under `llvm/` and touches no toolchain.

## `self-update`: the tools themselves, from a release

`sinstall` builds the tools from a checkout; `self-update` fetches them from a
release, for the far more common machine that has no checkout. It replaces
`knvm`, the `kira` launcher, and the launcher's `kira-language-server` alias by
the same stage-then-rename that makes replacing a running `knvm` safe on unix,
and every tool is checked present before any is replaced, so a truncated
archive cannot leave half of them updated.

Installed toolchains are untouched and `current.toml` is not rewritten: the
tools select between toolchain versions, so updating them must not silently
move a user to a different compiler. `knvm install latest` is that decision,
and stays separate. Finding the tools already current exits 0 - it is the good
outcome of an update, and a scheduled run must not go red on it.

## `binstall`: the checkout as a toolchain

`knvm binstall` is the developer route: run inside a Kira checkout, it builds
`kira`, `kira-language-server`, `kira-desktop-runner`, and both host runtime
archives with cargo (dev profile), cross-builds the Web runtime archive, and
shapes them into the same tree a release unpacks to with the checkout's
`foundation/` beside them. It installs that tree on the `dev` channel, named by
the workspace's `[workspace.package] version`. The LLVM backend is a hard part
of every kira, so `binstall` discovers the managed LLVM and refuses up front —
naming the provisioning route — when no bundle exists. It goes through the same
staging, validation, and rename-into-place pipeline as a release install, and
selects what it lands, so `kira` dispatches to the fresh build immediately.

Running it again replaces the installed tree. A dev toolchain names a moving
target, so `binstall` never answers "already installed" — that would mean
"silently stale", which is the one thing a rebuild command must not be.

The enclosing checkout is found by walking up from the working directory for a
`Cargo.toml` with `foundation/package.kira` beside it — the same two markers
bundled discovery's checkout rule requires — and anything short of both is
refused, so `binstall` in the wrong directory cannot build and install
whatever workspace it happens to be standing in.

## `sinstall`: the tools themselves

`binstall` provisions a toolchain; `knvm sinstall` provisions the *tools* —
`knvm` and the `kira` launcher, which live outside any toolchain version
because they are what selects between versions. Run inside a checkout, it
builds both with cargo, lands them in `<kira-home>/bin` (replacing a running
`knvm` atomically, stage-then-rename), writes a `<kira-home>/env` script that
prepends that directory to `PATH`, and appends one source line to the startup
file *the user's shell actually reads*, chosen from `$SHELL` and created when
missing: `.zshenv` for zsh, `.bashrc` for bash, `.profile` otherwise. Chosen
from the shell, not from what exists, because a default macOS home has no
dotfiles at all and a line in `.profile` configures nothing for the zsh that
machine runs. Every part is idempotent — a second run replaces binaries and
appends nothing.

When run in a terminal, it finishes by replacing itself with a fresh login
shell that already has the tools on PATH, so `knvm` and `kira` work in the
very next prompt; a non-interactive caller (a script, CI) gets the exit code
and no shell.

From a bare machine with a checkout, the whole setup is:

```sh
cargo run -p kira-knvm -- sinstall   # tools on PATH
knvm binstall                        # this checkout as the dev toolchain
kira run program.kira
```

It finds the checkout by the same marker walk as `binstall` and refuses
anything that is not one.

## Channels

A channel is both the directory namespace and the feed a version came from.
`release` is the default and means GitHub releases not marked prerelease, with
semver tags (`v1.7.3` installs as `1.7.3`). `dev` means the prerelease feed,
typically date-versioned. `latest` resolves within one channel and never across
them, and versions on different channels are independent installs;
`current.toml` records both, so nothing has to search.

Version ordering is numeric-aware but is **not** semver: a trailing prerelease
tag compares as text, so `1.7.0-rc1` sorts above `1.7.0`. That is harmless
while prereleases live on their own channel instead of in the tag, and needs a
real semver parser if such tags ever ship on `release`.

## Where releases come from

Everything downstream of the transport - extract, validate, move, select - is
one code path, reached through a `ReleaseSource` trait with two operations:
list the versions on a channel, and materialize one version's archive locally.

`GitHubReleaseSource` is the default, reading the releases API of
`kira-lang-com/kira` and picking the asset named
`kira-<version>-<host-key>.tar.gz`, where the host key is the same set
`kira-toolchain` already uses for LLVM bundles (`aarch64-macos`,
`x86_64-linux-gnu`, `x86_64-windows-msvc`). Transport is a single isolated
`curl` subprocess rather than an HTTP crate, which keeps the workspace's
external dependencies frozen; `curl` ships with macOS, Linux distributions, and
modern Windows, and its absence is a typed error rather than a panic.

`DirectoryReleaseSource` reads a directory laid out as
`<root>/<channel>/<version>/kira-<version>-<host-key>.tar.gz` and copies the
file. Setting `KNVM_RELEASE_DIR` points the binary at one, which is both the
offline-install route and what the tests drive: they build a fixture toolchain,
archive it with `tar`, and install it, exercising the shipped pipeline with
only the transport substituted.

`KIRA_HOME` relocates the whole `~/.kira` root, following the
`KIRA_FOUNDATION_HOME` / `KIRA_LLVM_HOME` precedent. It is what lets an install
be driven against a throwaway directory instead of a developer's real home, and
it reaches `current.toml` and Foundation discovery without either knowing about
it.

## How a release is published

Pushing a `v*` tag runs `.github/workflows/release.yml`, which builds each
supported host and attaches two archives per platform to a GitHub release, each
with its `.sha256` beside it: the toolchain archive named exactly as
`select_asset` expects, and `knvm-<version>-<host-key>.tar.gz` holding
`bin/knvm`, `bin/kira`, and the launcher's `bin/kira-language-server` alias —
what the bootstrap scripts and `self-update` fetch, and the one manual
download. Unpack that, put its `bin` on PATH, and `knvm install latest`
provisions the rest. A `-dev` tag publishes as a GitHub prerelease, which is
the `dev` channel.

The LLVM bundles are published separately, once per pin, by
`release-llvm-toolchains.yml` under the release tag `llvm-metadata.toml` names
— also with a `.sha256` each, which is what `install-llvm` verifies.

Nothing uploads until the same job has installed from the just-packed archive
with the just-built `knvm` (`KNVM_RELEASE_DIR`, throwaway `KIRA_HOME`) and run
the installed compiler through the `kira` launcher on an `import Foundation`
program. A tag that would publish a broken toolchain fails in that step
instead of shipping.

## What knvm does not touch

`libffi/` lives under the same `toolchains/` root but is keyed by its own
version and shared across toolchain versions, so a toolchain install must not
mutate it - nor `llvm/`, which only `install-llvm` writes. A toolchain install
writes exactly `<channel>/<version>/` and `current.toml` and nothing else; a
test pre-seeds witness files in both subtrees and asserts they survive.
Provisioning libffi is still knvm's job eventually.

## Not built, and honest about it

The GitHub transport has never been executed. Feed parsing, by-tag parsing,
channel mapping, tag stripping, asset selection, and sidecar reading are
unit-tested against canned API JSON, but no test opens a network connection,
and nothing has been published at `kira-lang-com/kira` to check against yet.
The release workflow produces artifacts named to the same contract, and its
verify step proves the archive installs — but the `curl`-against-the-live-API
path stays unexercised until a real tag is pushed and a real
`knvm install latest` runs against it. That covers `install`, `install-llvm`,
`self-update`, `list --remote`, and both bootstrap scripts equally: every one
of them is exercised only down to the transport.

Windows in that workflow is the first execution this project's install path
gets on that platform; a verify-step failure there is a finding, not noise.
`install.ps1` has never run at all.

The bootstrap scripts fetch from
`https://github.com/<repo>/releases/download/v<version>/…` and are documented
as living at `https://kira-lang.com/install.sh`, which nothing serves yet.
Until that URL exists the bootstrap is a file in this repo rather than a route
a user can take.

Checksums are the only integrity check: there are no signatures, so a verified
download proves the bytes match what the release published, not who published
them.

Still deferred: provisioning the libffi bundle, and a `knvm self-update` that
can roll back to the version it replaced.
