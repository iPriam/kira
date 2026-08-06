# Builds the toolchain MCP server, then runs it from a copy of the executable.
#
# The PowerShell twin of `mcp-server.sh`, for a host with no POSIX shell. Both
# exist for the same reason the other script pairs in this directory do: the
# repository is developed on both, and a developer should not need the other
# one installed to start their editor.
#
# Why a copy at all: the server is a long-lived process started by the editor
# and left running for a whole session. Run straight out of `target/`, its own
# executable is the file the next `cargo build --workspace` has to replace — and
# a running image cannot be replaced on Windows, so an ordinary build fails on
# the one binary the developer is not even working on:
#
#     error: failed to remove file `target\debug\kira-mcp.exe`
#     Caused by: Access is denied. (os error 5)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

# Cargo's own output goes to stderr; the redirect is what guarantees stdout
# carries nothing but JSON-RPC, which the client parses and one stray line ends.
cargo build --quiet --manifest-path (Join-Path $root 'Cargo.toml') `
    --package kira-mcp --bin kira-mcp 2>&1 | ForEach-Object { [Console]::Error.WriteLine($_) }
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$suffix = if ($IsWindows -or $env:OS -eq 'Windows_NT') { '.exe' } else { '' }
$built = Join-Path $root "target/debug/kira-mcp$suffix"
if (-not (Test-Path $built)) {
    [Console]::Error.WriteLine("kira-mcp: no server at $built after building it")
    exit 1
}

# One directory per process, so two editors open on this checkout do not race to
# write the same copy — and so a stale copy from a killed session never runs.
$runDir = Join-Path ([System.IO.Path]::GetTempPath()) "kira-mcp-run-$PID"
New-Item -ItemType Directory -Force -Path $runDir | Out-Null
$copy = Join-Path $runDir "kira-mcp$suffix"
Copy-Item -Path $built -Destination $copy -Force

try {
    & $copy @args
    exit $LASTEXITCODE
} finally {
    Remove-Item -Recurse -Force $runDir -ErrorAction SilentlyContinue
}
