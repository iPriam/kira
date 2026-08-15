# Run from any directory. `-List` writes the same JSON plan without starting a child.
# Filters accept PowerShell wildcards: `-Repository`, `-Target`, `-Mode`, `-Backend`.
[CmdletBinding()]
param(
    [Alias('Repo')]
    [string[]]$Repository = @('*'),
    [Alias('TargetName', 'TargetFilter')]
    [string[]]$Target = @('*'),
    [Alias('ModeFilter')]
    [ValidateSet('run', 'live')]
    [string[]]$Mode = @('run', 'live'),
    [Alias('BackendFilter')]
    [ValidateSet('vm', 'llvm', 'hybrid')]
    [string[]]$Backend = @('vm', 'llvm', 'hybrid'),
    [Alias('Timeout')]
    [ValidateRange(1, 86400)]
    [int]$TimeoutSeconds = 120,
    [Alias('QuitAfter')]
    [ValidatePattern('^\d+(ms|s|m)$')]
    [string]$LiveQuitAfter = '30s',
    [Alias('OutputPath')]
    [string]$JsonPath = '',
    [Alias('DryRun')]
    [switch]$List
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$moduleDirectory = Join-Path $PSScriptRoot 'external-surface'
. (Join-Path $moduleDirectory 'discovery.ps1')
. (Join-Path $moduleDirectory 'matrix.ps1')
. (Join-Path $moduleDirectory 'process.ps1')
. (Join-Path $moduleDirectory 'report.ps1')

$modeOrder = @('run', 'live')
$backendOrder = @('vm', 'llvm', 'hybrid')
$failureTableLimit = 40

$scriptDirectory = [System.IO.Path]::GetFullPath($PSScriptRoot)
$kiraRoot = [System.IO.Path]::GetFullPath((Join-Path $scriptDirectory '..'))
$projectsRoot = [System.IO.Path]::GetFullPath((Join-Path $kiraRoot '..'))

$rootDefinitions = @(
    [pscustomobject]@{
        Repository = 'kira-graphics'
        RelativeRoot = 'kira-graphics\examples'
        RootKind = 'container'
    },
    [pscustomobject]@{
        Repository = 'ui-foundation'
        RelativeRoot = 'ui-foundation\Examples'
        RootKind = 'container'
    },
    [pscustomobject]@{
        Repository = 'kira-ui'
        RelativeRoot = 'kira-ui\Examples'
        RootKind = 'container'
    },
    [pscustomobject]@{
        Repository = 'harmony-browser'
        RelativeRoot = 'harmony-browser'
        RootKind = 'single'
    },
    [pscustomobject]@{
        Repository = 'project-matter'
        RelativeRoot = 'project-matter\apps'
        RootKind = 'container'
    }
)
$repositoryOrder = [ordered]@{}
for ($index = 0; $index -lt $rootDefinitions.Count; $index++) {
    $repositoryOrder[$rootDefinitions[$index].Repository] = $index
}

$hostIncompatibleTargetTriples = [ordered]@{
    'aarch64-ios-none' = 'iOS requires an Apple device or simulator host.'
    'aarch64-ios-simulator' = 'iOS Simulator requires an Apple host.'
    'aarch64-macos-none' = 'macOS requires an Apple host.'
    'aarch64-tvos-none' = 'tvOS requires an Apple host.'
    'aarch64-tvos-simulator' = 'tvOS Simulator requires an Apple host.'
    'aarch64-xros-none' = 'visionOS requires an Apple host.'
    'aarch64-xros-simulator' = 'visionOS Simulator requires an Apple host.'
    'wasm32-emscripten-unknown' = 'WebAssembly requires a browser/Web device, not a Windows host process.'
    'wasm64-emscripten-unknown' = 'WebAssembly requires a browser/Web device, not a Windows host process.'
    'x86_64-linux-gnu' = 'Linux requires a Linux host.'
    'x86_64-macos-none' = 'macOS requires an Apple host.'
}

$buildTargetTriples = [ordered]@{
    Host = 'x86_64-windows-msvc'
    Wasm32 = 'wasm32-emscripten-unknown'
    Wasm64 = 'wasm64-emscripten-unknown'
}

$liveQuitAfterMilliseconds = $null
try {
    $durationMatch = [regex]::Match($LiveQuitAfter, '^(?<amount>\d+)(?<unit>ms|s|m)$')
    if (-not $durationMatch.Success) {
        throw 'live quit-after must use an integer duration such as 500ms, 5s, or 2m'
    }
    $amount = [int64]::Parse($durationMatch.Groups['amount'].Value)
    if ($amount -le 0) {
        throw 'live quit-after must be greater than zero'
    }
    $scale = switch ($durationMatch.Groups['unit'].Value) {
        'ms' { [int64]1 }
        's' { [int64]1000 }
        'm' { [int64]60000 }
    }
    if ($amount -gt [int64]::MaxValue / $scale) {
        throw 'live quit-after is too large'
    }
    $liveQuitAfterMilliseconds = $amount * $scale
    if ($liveQuitAfterMilliseconds -ge ([int64]$TimeoutSeconds * 1000)) {
        throw 'live quit-after must be shorter than the child timeout'
    }
}
catch {
    throw "invalid live quit-after '$LiveQuitAfter': $($_.Exception.Message)"
}

$selectedModes = @($modeOrder | Where-Object { $Mode -contains $_ })
$selectedBackends = @($backendOrder | Where-Object { $Backend -contains $_ })
if ($selectedModes.Count -eq 0) {
    throw 'the mode filter selected no supported modes'
}
if ($selectedBackends.Count -eq 0) {
    throw 'the backend filter selected no supported backends'
}

if ([string]::IsNullOrWhiteSpace($JsonPath)) {
    $JsonPath = Join-Path $kiraRoot '.codex\tmp\external-surface-matrix.json'
}
$resolvedJsonPath = Resolve-JsonPath -Path $JsonPath

$discovery = Find-MatrixTargets `
    -RootDefinitions $rootDefinitions `
    -ProjectsRoot $projectsRoot `
    -RepositoryPatterns $Repository `
    -TargetPatterns $Target `
    -RepositoryOrder $repositoryOrder `
    -BuildTargetTriples $buildTargetTriples `
    -HostIncompatibleTargetTriples $hostIncompatibleTargetTriples

$discoveredTargets = @($discovery.DiscoveredTargets)
$allTargets = @($discovery.SelectedTargets)
$exclusions = @($discovery.Exclusions)
$notManifestBacked = @($discovery.NotManifestBacked)
$discoveryErrors = @($discovery.DiscoveryErrors)
$selectedDefinitions = @($discovery.SelectedDefinitions)

$plannedCells = @()
foreach ($targetRecord in $allTargets) {
    foreach ($cellMode in $selectedModes) {
        foreach ($cellBackend in $selectedBackends) {
            $plannedCells += New-Cell -TargetRecord $targetRecord -CellMode $cellMode -CellBackend $cellBackend -QuitAfter $LiveQuitAfter
        }
    }
}

$kiraPath = $null
$kiraLookupError = $null
if (-not $List) {
    try {
        $kiraCommand = Get-Command -Name 'kira' -CommandType Application -ErrorAction Stop | Select-Object -First 1
        $kiraPath = if ($kiraCommand.Path) { $kiraCommand.Path } else { $kiraCommand.Source }
        if ([string]::IsNullOrWhiteSpace($kiraPath)) {
            throw 'PATH lookup returned no executable path'
        }
    }
    catch {
        $kiraLookupError = "kira was not found as an application on PATH: $($_.Exception.Message)"
    }
}

if ($List) {
    $results = @($plannedCells)
    $reportMode = 'list'
}
elseif ($kiraLookupError) {
    $results = @($plannedCells | ForEach-Object { New-UnavailableCellResult -Cell $_ -Reason $kiraLookupError })
    $reportMode = 'run'
}
else {
    $results = @($plannedCells | ForEach-Object {
        Write-Host "Running $($_.Repository)/$($_.Target) [$($_.Mode)/$($_.Backend)]"
        Invoke-MatrixCell -Cell $_ -KiraPath $kiraPath -TimeoutMilliseconds ($TimeoutSeconds * 1000)
    })
    $reportMode = 'run'
}

$selectedTargetCount = $allTargets.Count
$selectedCellCount = $plannedCells.Count
$passedCount = @($results | Where-Object { $_.Success -eq $true }).Count
$failedCount = if ($List) { 0 } else { @($results | Where-Object { $_.Success -ne $true }).Count }
$selectionError = $null
if ($selectedTargetCount -eq 0) {
    $selectionError = 'repository/target filters selected no manifest-backed App targets'
}
elseif ($selectedCellCount -eq 0) {
    $selectionError = 'mode/backend filters selected no matrix cells'
}

$report = [ordered]@{
    Schema = 'kira.external-surface-matrix.v1'
    GeneratedAtUtc = [DateTime]::UtcNow.ToString('o')
    Mode = $reportMode
    Host = [ordered]@{
        OperatingSystem = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
        Architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        TargetTriple = 'x86_64-windows-msvc'
        KiraExecutable = 'kira'
        KiraPath = $kiraPath
        ProjectsRoot = $projectsRoot
        KiraRoot = $kiraRoot
    }
    Filters = [ordered]@{
        Repository = @($Repository)
        Target = @($Target)
        Mode = @($selectedModes)
        Backend = @($selectedBackends)
        TimeoutSeconds = $TimeoutSeconds
        LiveQuitAfter = $LiveQuitAfter
    }
    Discovery = [ordered]@{
        Roots = @($selectedDefinitions | ForEach-Object {
            [ordered]@{
                Repository = $_.Repository
                Root = [System.IO.Path]::GetFullPath((Join-Path $projectsRoot $_.RelativeRoot))
            }
        })
        ManifestBackedAppCount = $discoveredTargets.Count
        SelectedTargetCount = $selectedTargetCount
        TargetCount = $selectedTargetCount
        ExclusionCount = $exclusions.Count
        Exclusions = @($exclusions)
        NotManifestBackedCount = $notManifestBacked.Count
        NotManifestBacked = @($notManifestBacked)
        ErrorCount = $discoveryErrors.Count
        Errors = @($discoveryErrors)
    }
    Matrix = [ordered]@{
        ModeCount = $selectedModes.Count
        BackendCount = $selectedBackends.Count
        CellCount = $selectedCellCount
        Results = @($results)
    }
    Summary = [ordered]@{
        Passed = $passedCount
        Failed = $failedCount
        SelectionError = $selectionError
        Status = if ($List) {
            'listed'
        }
        elseif ($failedCount -eq 0 -and $discoveryErrors.Count -eq 0 -and $null -eq $selectionError) {
            'passed'
        }
        else {
            'failed'
        }
    }
}

Write-JsonReport -Report $report -Path $resolvedJsonPath

Write-Output "Kira external-surface matrix: $reportMode"
Write-Output "Projects root: $projectsRoot"
Write-Output "Targets discovered/selected: $($discoveredTargets.Count)/$selectedTargetCount"
Write-Output "Modes: $($selectedModes -join ', ')"
Write-Output "Backends: $($selectedBackends -join ', ')"
Write-Output "Cells: $selectedCellCount"
Write-Output "JSON: $resolvedJsonPath"

if ($notManifestBacked.Count -gt 0) {
    Write-Output "Not manifest-backed (reported, not silently skipped): $($notManifestBacked.Count)"
    foreach ($entry in $notManifestBacked) {
        Write-Output "  $($entry.Repository)/$($entry.RelativePath) - $($entry.Reason)"
    }
}
if ($exclusions.Count -gt 0) {
    Write-Output "Explicit exclusions: $($exclusions.Count)"
    foreach ($exclusion in $exclusions) {
        $triple = if ($exclusion.Triple) { " [$($exclusion.Triple)]" } else { '' }
        Write-Output "  $($exclusion.Repository)/$($exclusion.RelativeTargetPath) - $($exclusion.Type)${triple}: $($exclusion.Reason)"
    }
}
if ($discoveryErrors.Count -gt 0) {
    Write-Output "Discovery errors: $($discoveryErrors.Count)"
    foreach ($errorRecord in $discoveryErrors) {
        Write-Output "  $($errorRecord.Repository) - $($errorRecord.Type): $($errorRecord.Message)"
    }
}
if ($selectionError) {
    Write-Output "Selection error: $selectionError"
}

if ($List) {
    Write-Output 'Cells:'
    foreach ($cell in $plannedCells) {
        Write-Output "  $($cell.Repository)/$($cell.Target) | $($cell.Mode) | $($cell.Backend) | cwd=$($cell.WorkingDirectory) | $($cell.Command)"
    }
    if ($discoveryErrors.Count -gt 0 -or $selectionError) {
        exit 1
    }
    exit 0
}

Write-FailureTable -Results $results -Limit $failureTableLimit
if ($discoveryErrors.Count -gt 0 -or $selectionError -or $failedCount -gt 0) {
    exit 1
}
exit 0
