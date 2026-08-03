# The Windows bootstrap: puts `knvm` and the `kira` launcher on PATH.
#
#   irm https://kira-lang.com/install.ps1 | iex
#
# The counterpart of `install.sh`, and deliberately the same shape: fetch the
# published tools archive, verify it against the checksum published beside it,
# unpack it into `<kira-home>\bin`, and put that directory on the user's PATH.
# It installs no toolchain; the last thing it prints is `knvm install latest`.
#
# Environment:
#   KIRA_HOME             where the tools land (default: $HOME\.kira)
#   KIRA_VERSION          which release to install (default: the newest)
#   KIRA_REPO             the repository to fetch from
#   KIRA_NO_MODIFY_PATH   set to any value to skip editing the user PATH

$ErrorActionPreference = 'Stop'

function Say($message) { Write-Host "kira: $message" }
function Fail($message) { Write-Error "kira: $message"; exit 1 }

$repo = if ($env:KIRA_REPO) { $env:KIRA_REPO } else { 'kira-lang-com/kira' }
$kiraHome = if ($env:KIRA_HOME) { $env:KIRA_HOME } else { Join-Path $HOME '.kira' }
$binDir = Join-Path $kiraHome 'bin'

# The one host key published for Windows. An arm64 machine has no build yet,
# and saying so beats installing something that cannot run.
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    Fail "no Kira build is published for this architecture; build from a checkout with ``cargo run -p kira-knvm -- sinstall``"
}
$key = 'x86_64-windows-msvc'

$version = $env:KIRA_VERSION
if (-not $version) {
    $feed = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases?per_page=10" `
        -Headers @{ 'User-Agent' = 'kira-install'; 'Accept' = 'application/vnd.github+json' }
    $version = ($feed | Select-Object -First 1).tag_name -replace '^v', ''
}
if (-not $version) { Fail "could not resolve the newest release from $repo" }

$asset = "knvm-$version-$key.tar.gz"
$base = "https://github.com/$repo/releases/download/v$version"

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("kira-install-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $work -Force | Out-Null
try {
    Say "downloading $asset"
    $archive = Join-Path $work $asset
    Invoke-WebRequest -Uri "$base/$asset" -OutFile $archive -UseBasicParsing

    $sidecar = Join-Path $work "$asset.sha256"
    $havePublished = $true
    try {
        Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile $sidecar -UseBasicParsing
    }
    catch {
        $havePublished = $false
    }
    if ($havePublished) {
        $published = ((Get-Content $sidecar -Raw).Trim() -split '\s+')[0]
        $actual = (Get-FileHash -Path $archive -Algorithm SHA256).Hash.ToLower()
        if ($published.ToLower() -ne $actual) {
            Fail "checksum mismatch for $asset`n  published: $published`n  downloaded: $actual`nThe download is corrupt or the archive was changed after publication; nothing was installed"
        }
        Say "verified sha256 $actual"
    }
    else {
        Say "warning: no checksum is published for $asset; installed unverified"
    }

    # Windows has shipped bsdtar as `tar` since 1803, which is the same
    # baseline knvm's own transport assumes for `curl`.
    $unpacked = Join-Path $work 'unpacked'
    New-Item -ItemType Directory -Path $unpacked -Force | Out-Null
    tar -xzf $archive -C $unpacked
    if ($LASTEXITCODE -ne 0) { Fail "could not unpack $asset" }

    $payload = Join-Path $unpacked "knvm-$version"
    if (-not (Test-Path (Join-Path $payload 'bin'))) { $payload = $unpacked }
    if (-not (Test-Path (Join-Path $payload 'bin'))) { Fail "$asset does not contain a bin directory" }

    New-Item -ItemType Directory -Path $binDir -Force | Out-Null
    $tools = @('knvm.exe', 'kira.exe', 'kira-language-server.exe')
    foreach ($tool in $tools) {
        if (-not (Test-Path (Join-Path $payload "bin\$tool"))) { Fail "$asset has no bin\$tool" }
    }
    foreach ($tool in $tools) {
        Copy-Item -Path (Join-Path $payload "bin\$tool") -Destination (Join-Path $binDir $tool) -Force
    }
    Say "installed knvm, kira, and kira-language-server into $binDir"

    if ($env:KIRA_NO_MODIFY_PATH) {
        Say "not editing PATH; add $binDir yourself"
    }
    else {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        if ($userPath -split ';' -contains $binDir) {
            Say "$binDir is already on your PATH"
        }
        else {
            $newPath = if ($userPath) { "$binDir;$userPath" } else { $binDir }
            # The first write is the one that broadcasts WM_SETTINGCHANGE, so a
            # terminal opened afterwards sees this without a sign-out. It stores
            # the value as REG_SZ, so the second restores the expandable type
            # that entries like %USERPROFILE%\bin need to keep working.
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            Set-ItemProperty -Path 'HKCU:\Environment' -Name 'Path' -Value $newPath -Type ExpandString
            Say "added $binDir to your user PATH"
        }
    }

    Say "done. Open a new terminal, then install a toolchain with: knvm install latest"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
