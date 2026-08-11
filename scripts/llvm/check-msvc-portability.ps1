param(
    [Parameter(Mandatory = $true)]
    [string]$InstallDir
)

$ErrorActionPreference = "Stop"

# The MSVC STL compiles a growing set of algorithm helpers into the toolset's
# own runtime library rather than into the headers, and names them `__std_*`.
# A static archive that references one links only against a Visual Studio at
# least as new as the one that built it. This bundle is redistributable and is
# linked on developer machines whose Visual Studio nobody controls, so every
# such reference outside the list below is a link failure waiting for whoever
# has not upgraded yet.
#
# These are the helpers that have shipped since Visual Studio 2015, which is
# the floor the bundle promises. Checking the built tree rather than pinning
# the toolset that builds it is what keeps the promise independent of the
# runner image: the image may update whenever GitHub updates it, and the
# property that actually matters is proven here on every build.
$sinceVisualStudio2015 = @(
    "__std_exception_copy",
    "__std_exception_destroy",
    "__std_init_once_begin_initialize",
    "__std_init_once_complete",
    "__std_init_once_link_alternate_names_and_abort",
    "__std_system_error_allocate_message",
    "__std_system_error_deallocate_message",
    "__std_terminate",
    "__std_type_info_compare",
    "__std_type_info_destroy_list",
    "__std_type_info_name"
)

$nm = Join-Path $InstallDir "bin\llvm-nm.exe"
if (-not (Test-Path $nm)) {
    throw "the LLVM built into $InstallDir ships no bin\llvm-nm.exe, so its archives cannot be read"
}

$libDir = Join-Path $InstallDir "lib"
$libs = @(Get-ChildItem -Path $libDir -Filter *.lib -File)
if ($libs.Count -eq 0) {
    throw "the LLVM built into $InstallDir installed no libraries under lib"
}

$offenders = [ordered]@{}
$baselineHits = 0
foreach ($lib in $libs) {
    $symbols = & $nm --undefined-only $lib.FullName
    if ($LASTEXITCODE -ne 0) {
        throw "llvm-nm could not read $($lib.Name)"
    }
    foreach ($line in $symbols) {
        if ($line -notmatch '^\s*U\s+(__std_[A-Za-z0-9_]+)') { continue }
        $symbol = $Matches[1]
        if ($sinceVisualStudio2015 -contains $symbol) {
            $baselineHits++
            continue
        }
        if (-not $offenders.Contains($symbol)) {
            $offenders[$symbol] = $lib.Name
        }
    }
}

# Every LLVM links against the separately compiled STL somewhere, so a scan
# that finds no `__std_*` reference at all read something other than the
# bundle's object code, and a gate that cannot fail is not a gate.
if ($baselineHits -eq 0 -and $offenders.Count -eq 0) {
    throw "no __std_ reference was found in any of the $($libs.Count) libraries under $libDir, so this check read nothing it could judge"
}

if ($offenders.Count -gt 0) {
    Write-Output "The built LLVM references STL helpers that only the toolset which compiled it defines:"
    foreach ($symbol in $offenders.Keys) {
        Write-Output ("  {0}  (first seen in {1})" -f $symbol, $offenders[$symbol])
    }
    Write-Output ""
    Write-Output "Consumers on an older Visual Studio cannot resolve these. Opt the family"
    Write-Output "out in scripts/llvm/build-llvm.ps1, the way _USE_STD_VECTOR_ALGORITHMS=0"
    Write-Output "opts out of the vectorized algorithms, or add the symbol to this script's"
    Write-Output "baseline if it predates Visual Studio 2015."
    exit 1
}

Write-Output "$($libs.Count) libraries reference only STL helpers that have shipped since Visual Studio 2015."
