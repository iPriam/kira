#!/usr/bin/env sh
# Builds the debug-session MCP server, then runs it from a copy of the
# executable.
#
# The same shape as `mcp-server.sh`, for the same reason: the server is a
# long-lived process the editor leaves running for a whole session, and a
# running image cannot be replaced on Windows, so a server run straight out of
# `target/` breaks the next `cargo build --workspace`.
#
# It builds `kira` too, because that is what the server compiles a Kira program
# with, and points `KIRA_EXECUTABLE` at that build rather than at whatever
# happens to be on PATH.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# Cargo's progress goes to stderr already, but `--quiet` plus an explicit
# redirect is what guarantees stdout carries nothing but JSON-RPC: the client
# parses this stream, and one stray line ends the session.
cargo build --quiet --manifest-path "$root/Cargo.toml" \
    --package kira-lldb-mcp --bin kira-lldb-mcp \
    --package kira-cli --bin kira >&2

case "$(uname -s 2>/dev/null || echo unknown)" in
    MINGW* | MSYS* | CYGWIN* | Windows_NT) suffix=.exe ;;
    *) suffix= ;;
esac

built="$root/target/debug/kira-lldb-mcp$suffix"
if [ ! -f "$built" ]; then
    echo "kira-lldb-mcp: no server at $built after building it" >&2
    exit 1
fi
KIRA_EXECUTABLE="${KIRA_EXECUTABLE:-$root/target/debug/kira$suffix}"
export KIRA_EXECUTABLE

# Left behind by sessions that were killed rather than closed, which is most of
# them: an editor exiting does not give this script a chance to tidy up. Swept
# on the way in instead. A directory still in use resists deletion on the host
# where that matters, so a live sibling's copy is skipped rather than pulled out
# from under it.
for stale in "${TMPDIR:-/tmp}"/kira-lldb-mcp-run-*; do
    [ -d "$stale" ] && rm -rf "$stale" 2>/dev/null
done || true

# One directory per process, so two editors open on this checkout do not race
# to write the same copy.
run_dir="${TMPDIR:-/tmp}/kira-lldb-mcp-run-$$"
mkdir -p "$run_dir"
copy="$run_dir/kira-lldb-mcp$suffix"
cp "$built" "$copy"

# `exec`, so the copy *is* this process: same pid for the client to watch, same
# stdio, and no shell left in the middle.
exec "$copy" "$@"
