<#
.SYNOPSIS
Fails when a built LLVM bundle is not the machine type its target key names.

.DESCRIPTION
A bundle built for the wrong architecture is the worst shape an artifact takes:
it installs, it is the right size, its archives hold every symbol a reader
looks for, and the only thing that ever objects is a linker on the target host
reporting all of them unresolved — naming the consumer's own functions and
nothing about the bundle.

That is what shipped once. `ilammy/msvc-dev-cmd` defaults to `x64` whatever it
runs on, so an arm64 runner with no `arch` produced an x86_64 bundle under an
`aarch64-windows-msvc` name.

The bundle's own tools are the evidence: an ARM64 bundle's `llvm-config.exe` is
an ARM64 image. This reads the PE header rather than asking a tool, so it needs
nothing installed and cannot be fooled by whatever is on PATH.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$InstallDir,
    [Parameter(Mandatory = $true)]
    [string]$TargetKey
)

$ErrorActionPreference = "Stop"

# The COFF machine types a Windows bundle may legitimately be.
$machines = @{
    0x8664 = "x86_64"
    0xAA64 = "aarch64"
    0xA641 = "aarch64ec"
    0xA64E = "aarch64x"
}

$expected = if ($TargetKey.StartsWith("aarch64")) { "aarch64" } else { "x86_64" }

$probe = Join-Path $InstallDir "bin\llvm-config.exe"
if (-not (Test-Path $probe)) {
    throw "the bundle has no bin\llvm-config.exe to read a machine type from: $probe"
}

$bytes = [System.IO.File]::ReadAllBytes($probe)
# DOS header: `e_lfanew` at 0x3C points at the PE signature; the machine type is
# the first field of the COFF header that follows it.
$peOffset = [System.BitConverter]::ToInt32($bytes, 0x3C)
$signature = [System.BitConverter]::ToUInt32($bytes, $peOffset)
if ($signature -ne 0x00004550) {
    throw "$probe is not a PE image (no PE signature at 0x$($peOffset.ToString('X')))"
}
$machine = [System.BitConverter]::ToUInt16($bytes, $peOffset + 4)
$actual = $machines[[int]$machine]
if (-not $actual) {
    $actual = "0x{0:X4}" -f $machine
}

if ($actual -ne $expected) {
    throw @"
this bundle is named for $TargetKey and was built as $actual.

`llvm-config.exe` is a $actual image, so every archive beside it is too. A
consumer linking this bundle on $expected gets every LLVM symbol reported
unresolved, naming its own functions and nothing about the bundle.

The usual cause is the developer environment: `ilammy/msvc-dev-cmd` defaults to
`x64` on every runner, so an arm64 job without an explicit `arch` builds x86_64.
"@
}

Write-Host "bundle machine type is $actual, as $TargetKey requires"
