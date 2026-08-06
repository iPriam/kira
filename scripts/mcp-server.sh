#!/usr/bin/env sh
# Builds the toolchain MCP server, then runs it from a copy of the executable.
#
# The server is a long-lived process started by the editor and left running for
# a whole session. Run straight out of `target/`, its own executable is the file
# the next `cargo build --workspace` has to replace — and a running image cannot
# be replaced on Windows, so an ordinary build fails on the one binary the
# developer is not even working on:
#
#     error: failed to remove file `target\debug\kira-mcp.exe`
#     Caused by: Access is denied. (os error 5)
#
# Building normally and running a copy fixes that without a second target
# directory: the build stays shared with every other crate, and the file cargo
# wants to overwrite is not the one held open.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# Cargo's progress goes to stderr already, but `--quiet` plus an explicit
# redirect is what guarantees stdout carries nothing but JSON-RPC: the client
# parses this stream, and one stray line ends the session.
cargo build --quiet --manifest-path "$root/Cargo.toml" \
    --package kira-mcp --bin kira-mcp >&2

case "$(uname -s 2>/dev/null || echo unknown)" in
    MINGW* | MSYS* | CYGWIN* | Windows_NT) suffix=.exe ;;
    *) suffix= ;;
esac

built="$root/target/debug/kira-mcp$suffix"
if [ ! -f "$built" ]; then
    echo "kira-mcp: no server at $built after building it" >&2
    exit 1
fi

# Left behind by sessions that were killed rather than closed, which is most of
# them: an editor exiting does not give this script a chance to tidy up. Swept
# on the way in instead, where there is one. A directory still in use resists
# deletion on the host where that matters, so a live sibling's copy is skipped
# rather than pulled out from under it.
for stale in "${TMPDIR:-/tmp}"/kira-mcp-run-*; do
    [ -d "$stale" ] && rm -rf "$stale" 2>/dev/null
done || true

# One directory per process, so two editors open on this checkout do not race
# to write the same copy.
run_dir="${TMPDIR:-/tmp}/kira-mcp-run-$$"
mkdir -p "$run_dir"
copy="$run_dir/kira-mcp$suffix"
cp "$built" "$copy"

# `exec`, so the copy *is* this process: same pid for the client to watch, same
# stdio, and no shell left in the middle. Backgrounding it instead would hand
# the server `/dev/null` for stdin — a background command's stdin is redirected
# when job control is off — and the session would end at the first read.
exec "$copy" "$@"
