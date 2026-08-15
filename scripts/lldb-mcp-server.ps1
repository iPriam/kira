# Builds the debug-session MCP server, then runs it from a copy of the
# executable.
#
# The PowerShell twin of `lldb-mcp-server.sh`, and the same shape as
# `mcp-server.ps1` for the same reason: the server is a long-lived process the
# editor leaves running for a whole session, and on Windows a running image
# cannot be replaced, so a build out of `target/` would break the next
# `cargo build --workspace`.
#
# It also builds `kira`, because that is what the server compiles a Kira
# program with, and points `KIRA_EXECUTABLE` at the build rather than at
# whatever happens to be on PATH.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

# Cargo's own output goes to stderr; the redirect is what guarantees stdout
# carries nothing but JSON-RPC, which the client parses and one stray line ends.
cargo build --quiet --manifest-path (Join-Path $root 'Cargo.toml') `
    --package kira-lldb-mcp --bin kira-lldb-mcp `
    --package kira-cli --bin kira 2>&1 | ForEach-Object { [Console]::Error.WriteLine($_) }
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$suffix = if ($IsWindows -or $env:OS -eq 'Windows_NT') { '.exe' } else { '' }
$built = Join-Path $root "target/debug/kira-lldb-mcp$suffix"
$compiler = Join-Path $root "target/debug/kira$suffix"
if (-not (Test-Path $built)) {
    [Console]::Error.WriteLine("kira-lldb-mcp: no server at $built after building it")
    exit 1
}
if (-not $env:KIRA_EXECUTABLE) { $env:KIRA_EXECUTABLE = $compiler }

# One directory per process, so two editors open on this checkout do not race to
# write the same copy — and so a stale copy from a killed session never runs.
$runDir = Join-Path ([System.IO.Path]::GetTempPath()) "kira-lldb-mcp-run-$PID"
New-Item -ItemType Directory -Force -Path $runDir | Out-Null
$copy = Join-Path $runDir "kira-lldb-mcp$suffix"
Copy-Item -Path $built -Destination $copy -Force

try {
    & $copy @args
    exit $LASTEXITCODE
} finally {
    Remove-Item -Recurse -Force $runDir -ErrorAction SilentlyContinue
}
