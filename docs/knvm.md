# knvm

`knvm` installs Kira. It is the first thing a user runs, before any `kirac`
exists on the machine:

```sh
knvm install latest
knvm install 1.7.3 --channel dev
knvm binstall
knvm sinstall
knvm list
knvm use 1.7.3
knvm uninstall 1.7.3
```

`install` fetches a release, unpacks it into
`~/.kira/toolchains/<channel>/<version>/`, validates it, and selects it by
writing `current.toml`. The other three verbs operate on what is already on
disk and never reach a network. `--channel` defaults to `release` everywhere,
and `switch` is accepted as a spelling of `use`.

Selection is what the `kira` launcher reads. It resolves `current.toml`, finds
the selected toolchain's primary binary, and hands the process over with every
argument and the whole environment forwarded untouched: on unix by replacing
its own process image, so the exit status, signals, and stdio belong to `kirac`
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
 current.toml channel = "release", version = "1.10.0", primary = "kirac"
 release/1.10.0/
 bin/kirac
 bin/kira-language-server the editor server, same frontend as its kirac
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
consumer that is not `kirac` itself, such as a `build.rs` running through
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
process alone, unpacked with the system `tar`, and validated: the tree must
hold `bin/kirac` and `bin/kira-language-server`, both executable on unix, and
`foundation/package.kira` must exist. The language server is required on
purpose: a toolchain without one leaves an editor silently running whatever
stale server it finds elsewhere on PATH. Only then is the validated tree moved
into place with a single `rename`, and `current.toml` written. Nothing appears
under `<channel>/` until the toolchain is known-good, so a failed install
leaves no half-toolchain a launcher could dispatch to. Staging is removed on
every exit path.

The one hole in that guarantee is `SIGKILL`: the staging cleanup is a `Drop`
guard, which a hard kill does not run, leaving an inert directory under
`.staging/` that nothing prunes.

## Listing, selecting, removing

`list` walks the channel directories, reports `release` before `dev` and newest
first within each, and marks the selected one with `*`. A toolchains root that
does not exist is an empty listing rather than an error - it is the state of a
machine that has never run `knvm install`. A version whose tree has lost its
`bin/kirac` is listed and flagged broken instead of being hidden.

`use` refuses a version that is not installed, and equally refuses one missing
the binary the launcher runs: selecting either would leave `kira` unable to
dispatch, which is better reported now than at the next invocation.

`uninstall` removes exactly `<channel>/<version>/`. When the removed version
was the selected one, `current.toml` is deleted and the removal warns that
nothing is selected - repointing at some surviving version would silently
change which compiler a user runs.

## `binstall`: the checkout as a toolchain

`knvm binstall` is the developer route: run inside a Kira checkout, it builds
`kirac` and `kira-language-server` with cargo (dev profile), shapes the result
into the same tree a release unpacks to — the built binaries under `bin/`, the
checkout's `foundation/` beside them — and installs it on the `dev` channel,
named by the workspace's `[workspace.package] version`. The LLVM backend is a
hard part of every kirac, so `binstall` discovers the managed LLVM, points
`llvm-sys` at it, and refuses up front — naming the provisioning route — when
no bundle exists. It goes through the
same staging, validation, and rename-into-place pipeline as a release install,
and selects what it lands, so `kira` dispatches to the fresh build
immediately.

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
supported host and attaches two archives per platform to a GitHub release: the
toolchain archive named exactly as `select_asset` expects, and
`knvm-<version>-<host-key>.tar.gz` holding `bin/knvm`, `bin/kira`, and the
launcher's `bin/kira-language-server` alias — the one
manual download. Unpack that, put its `bin` on PATH, and `knvm install latest`
provisions the rest. A `-dev` tag publishes as a GitHub prerelease, which is
the `dev` channel.

Nothing uploads until the same job has installed from the just-packed archive
with the just-built `knvm` (`KNVM_RELEASE_DIR`, throwaway `KIRA_HOME`) and run
the installed compiler through the `kira` launcher on an `import Foundation`
program. A tag that would publish a broken toolchain fails in that step
instead of shipping.

## What knvm does not touch

`llvm/` and `libffi/` live under the same `toolchains/` root but are keyed by
their own versions and shared across toolchain versions, so a toolchain install
must not mutate them. This build writes exactly `<channel>/<version>/` and
`current.toml` and nothing else; a test pre-seeds witness files in both
subtrees and asserts they survive. Provisioning those bundles is knvm's job
eventually - the LLVM backend already discovers them through
`kira_toolchain::discover()` with the `KIRA_LLVM_HOME` override, so nothing
user-visible is blocked on it.

## Not built, and honest about it

The GitHub transport has never been executed. Feed parsing, channel mapping,
tag stripping, and asset selection are unit-tested against canned API JSON, but
no test opens a network connection, and nothing has been published at
`kira-lang-com/kira` to check against yet. The release workflow produces
artifacts named to the same contract, and its verify step proves the archive
installs — but the `curl`-against-the-live-API path stays unexercised until a
real tag is pushed and a real `knvm install latest` runs against it.

Windows in that workflow is the first execution this project's install path
gets on that platform; a verify-step failure there is a finding, not noise.

Downloads are not verified beyond `curl -f` rejecting HTTP errors and `tar`
rejecting a corrupt archive. There is no checksum or signature check; the hook
belongs in `install` immediately after the fetch.

Also deferred: the `curl | sh` bootstrap that installs knvm itself, which now
has published binaries to fetch once a tag ships but still needs a hosted
script URL; listing versions
available remotely, as opposed to those installed; `knvm install-llvm`; knvm
self-update; and per-project toolchain pinning, since `current.toml` is a
single global selection.
