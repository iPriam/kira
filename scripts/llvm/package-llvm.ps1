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

switch ($ArchiveFormat) {
    "zip" {
        Compress-Archive -Path (Join-Path $InstallDir "*") -DestinationPath $archivePath -Force
    }
    "tar.xz" {
        & tar.exe -cJf $archivePath -C $InstallDir .
    }
}

Write-Output $archivePath
