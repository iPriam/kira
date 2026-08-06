param(
    [Parameter(Mandatory = $true)]
    [string]$InstallDir,
    [Parameter(Mandatory = $true)]
    [string]$OutputDir,
    [Parameter(Mandatory = $true)]
    [string]$AssetName,
    [Parameter(Mandatory = $true)]
    [ValidateSet("zip", "tar.xz")]
    [string]$ArchiveFormat
)

$ErrorActionPreference = "Stop"

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$archivePath = Join-Path $OutputDir $AssetName
if (Test-Path $archivePath) {
    Remove-Item $archivePath -Force
}

# `bsdtar` writes both formats and streams the tree once. `Compress-Archive`
# also writes zip, but reads the whole install tree into the pipeline first,
# which on a multi-gigabyte LLVM costs more than the build can spare against
# the runner's six-hour ceiling.
switch ($ArchiveFormat) {
    "zip" {
        & tar.exe -a -c -f $archivePath -C $InstallDir .
    }
    "tar.xz" {
        & tar.exe -cJf $archivePath -C $InstallDir .
    }
}
if ($LASTEXITCODE -ne 0) {
    throw "packaging $AssetName failed"
}

Write-Output $archivePath
