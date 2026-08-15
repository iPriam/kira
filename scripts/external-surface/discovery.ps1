function Test-FilterMatch {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value,
        [Parameter(Mandatory = $true)]
        [string[]]$Patterns
    )

    foreach ($pattern in $Patterns) {
        if ($Value -like $pattern) {
            return $true
        }
    }
    return $false
}

function Test-PathUnder {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Parent,
        [Parameter(Mandatory = $true)]
        [string]$Child
    )

    $parentFull = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\', '/')
    $childFull = [System.IO.Path]::GetFullPath($Child).TrimEnd('\', '/')
    if ($childFull.Equals($parentFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    $separator = [string][System.IO.Path]::DirectorySeparatorChar
    return $childFull.StartsWith($parentFull + $separator, [System.StringComparison]::OrdinalIgnoreCase)
}

function Get-RelativeTargetPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RepositoryRoot,
        [Parameter(Mandatory = $true)]
        [string]$TargetPath
    )

    $repositoryRootFull = [System.IO.Path]::GetFullPath($RepositoryRoot)
    $targetPathFull = [System.IO.Path]::GetFullPath($TargetPath)
    if (-not (Test-PathUnder -Parent $repositoryRootFull -Child $targetPathFull)) {
        throw "target path '$targetPathFull' is outside repository root '$repositoryRootFull'"
    }
    if ($targetPathFull.Equals($repositoryRootFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        return '.'
    }
    $relative = $targetPathFull.Substring($repositoryRootFull.Length).TrimStart('\', '/')
    return ($relative -replace '\\', '/')
}

function Remove-ManifestLineComments {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Text
    )

    $output = New-Object System.Text.StringBuilder
    $quoted = $false
    $escaped = $false
    $commented = $false

    for ($index = 0; $index -lt $Text.Length; $index++) {
        $character = $Text[$index]
        if ($commented) {
            if ($character -eq "`r" -or $character -eq "`n") {
                $commented = $false
                [void]$output.Append($character)
            }
            else {
                [void]$output.Append(' ')
            }
            continue
        }

        if ($quoted) {
            [void]$output.Append($character)
            if ($escaped) {
                $escaped = $false
            }
            elseif ($character -eq '\') {
                $escaped = $true
            }
            elseif ($character -eq '"') {
                $quoted = $false
            }
            continue
        }

        if ($character -eq '"') {
            $quoted = $true
            [void]$output.Append($character)
        }
        elseif ($character -eq '/' -and $index + 1 -lt $Text.Length -and $Text[$index + 1] -eq '/') {
            $commented = $true
            [void]$output.Append(' ')
            $index++
            [void]$output.Append(' ')
        }
        else {
            [void]$output.Append($character)
        }
    }

    return $output.ToString()
}

function Read-PackageManifest {
    param(
        [Parameter(Mandatory = $true)]
        [System.IO.FileInfo]$Manifest,
        [Parameter(Mandatory = $true)]
        [string]$Repository,
        [Parameter(Mandatory = $true)]
        [string]$RepositoryRoot,
        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary]$BuildTargetTriples,
        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary]$HostIncompatibleTargetTriples
    )

    $metadata = [ordered]@{
        Repository = $Repository
        RepositoryRoot = $RepositoryRoot
        ManifestPath = $Manifest.FullName
        RelativeTargetPath = Get-RelativeTargetPath -RepositoryRoot $RepositoryRoot -TargetPath $Manifest.DirectoryName
        TargetName = Split-Path -Leaf $Manifest.DirectoryName
        PackageName = $null
        Kind = $null
        BuildTarget = 'Host'
        TargetTriple = $null
        HostIncompatibleReason = $null
        ParseError = $null
    }

    try {
        $text = Get-Content -Raw -LiteralPath $Manifest.FullName
        $clean = Remove-ManifestLineComments -Text $text

        $header = [regex]::Match($clean, '(?s)\bPackage\s+(?<name>[A-Za-z_][A-Za-z0-9_]*)\s*\{')
        if (-not $header.Success) {
            throw 'expected a Package <name> { declaration'
        }
        $metadata.PackageName = $header.Groups['name'].Value

        $kindMatches = [regex]::Matches($clean, '(?m)\blet\s+kind\s*=\s*(?<value>[^\r\n]+)')
        if ($kindMatches.Count -gt 0) {
            $kindValue = $kindMatches[$kindMatches.Count - 1].Groups['value'].Value.Trim().TrimEnd(',')
            $kindCase = ($kindValue -split '\.')[-1].Trim()
            if ($kindCase -ieq 'App') {
                $metadata.Kind = 'App'
            }
            elseif ($kindCase -ieq 'Library') {
                $metadata.Kind = 'Library'
            }
            else {
                throw "unknown package kind '$kindValue'"
            }
        }
        else {
            $metadata.Kind = 'App'
        }

        $targetMatches = [regex]::Matches($clean, '(?m)\bbuildTarget\s*:\s*(?<value>[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)?)')
        if ($targetMatches.Count -gt 0) {
            $targetValue = $targetMatches[$targetMatches.Count - 1].Groups['value'].Value.Trim()
            $metadata.BuildTarget = ($targetValue -split '\.')[-1]
        }

        if (-not $BuildTargetTriples.Contains($metadata.BuildTarget)) {
            throw "unknown build target '$($metadata.BuildTarget)'"
        }

        $metadata.TargetTriple = $BuildTargetTriples[$metadata.BuildTarget]
        if ($metadata.TargetTriple -ne 'x86_64-windows-msvc' -and $HostIncompatibleTargetTriples.Contains($metadata.TargetTriple)) {
            $metadata.HostIncompatibleReason = $HostIncompatibleTargetTriples[$metadata.TargetTriple]
        }
    }
    catch {
        $metadata.ParseError = $_.Exception.Message
    }

    return [pscustomobject]$metadata
}

function Get-ManifestFiles {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,
        [Parameter(Mandatory = $true)]
        [string]$Repository,
        [Parameter(Mandatory = $true)]
        [string]$RepositoryRoot,
        [Parameter(Mandatory = $true)]
        [ValidateSet('container', 'single')]
        [string]$RootKind
    )

    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        return [pscustomobject]@{
            Files = @()
            NotManifestBacked = @()
            Error = [pscustomobject]@{
                Repository = $Repository
                Root = $Root
                Type = 'missing-root'
                Message = 'target discovery root does not exist'
            }
        }
    }

    try {
        if ($RootKind -eq 'single') {
            $singleManifest = Join-Path $Root 'package.kira'
            $files = @()
            if (Test-Path -LiteralPath $singleManifest -PathType Leaf) {
                $files = @(Get-Item -LiteralPath $singleManifest)
            }
            $notManifestBacked = if ($files.Count -eq 0) {
                @([pscustomobject]@{
                    Repository = $Repository
                    Root = $Root
                    Path = $Root
                    RelativePath = Get-RelativeTargetPath -RepositoryRoot $RepositoryRoot -TargetPath $Root
                    Reason = 'root has no package.kira'
                })
            }
            else {
                @()
            }
        }
        else {
            $files = @(
                Get-ChildItem -LiteralPath $Root -Recurse -File -Filter 'package.kira' -Force |
                    Where-Object {
                        $manifestDirectory = [System.IO.Path]::GetFullPath($_.DirectoryName)
                        Test-PathUnder -Parent $Root -Child $manifestDirectory
                    } |
                    Sort-Object FullName
            )
            $topLevel = @(Get-ChildItem -LiteralPath $Root -Directory -Force | Sort-Object FullName)
            $notManifestBacked = @()
            foreach ($directory in $topLevel) {
                $directoryPath = [System.IO.Path]::GetFullPath($directory.FullName)
                $hasManifest = @($files | Where-Object {
                    $manifestDirectory = [System.IO.Path]::GetFullPath($_.DirectoryName)
                    Test-PathUnder -Parent $directoryPath -Child $manifestDirectory
                }).Count -gt 0
                if (-not $hasManifest) {
                    $notManifestBacked += [pscustomobject]@{
                        Repository = $Repository
                        Root = $Root
                        Path = $directoryPath
                        RelativePath = Get-RelativeTargetPath -RepositoryRoot $RepositoryRoot -TargetPath $directoryPath
                        Reason = 'directory is not backed by package.kira'
                    }
                }
            }
        }

        return [pscustomobject]@{
            Files = @($files)
            NotManifestBacked = @($notManifestBacked)
            Error = $null
        }
    }
    catch {
        return [pscustomobject]@{
            Files = @()
            NotManifestBacked = @()
            Error = [pscustomobject]@{
                Repository = $Repository
                Root = $Root
                Type = 'discovery-error'
                Message = $_.Exception.Message
            }
        }
    }
}

function Test-TargetFilter {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RepositoryName,
        [Parameter(Mandatory = $true)]
        [string]$TargetName,
        [Parameter(Mandatory = $true)]
        [string]$RelativeTargetPath,
        [Parameter(Mandatory = $true)]
        [string[]]$Patterns
    )

    $qualifiedTarget = "$RepositoryName/$TargetName"
    $qualifiedPath = "$RepositoryName/$RelativeTargetPath"
    foreach ($pattern in $Patterns) {
        if ($TargetName -like $pattern -or
            $RelativeTargetPath -like $pattern -or
            $qualifiedTarget -like $pattern -or
            $qualifiedPath -like $pattern) {
            return $true
        }
    }
    return $false
}

function Find-MatrixTargets {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$RootDefinitions,
        [Parameter(Mandatory = $true)]
        [string]$ProjectsRoot,
        [Parameter(Mandatory = $true)]
        [string[]]$RepositoryPatterns,
        [Parameter(Mandatory = $true)]
        [string[]]$TargetPatterns,
        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary]$RepositoryOrder,
        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary]$BuildTargetTriples,
        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary]$HostIncompatibleTargetTriples
    )

    $discoveredTargets = @()
    $allTargets = @()
    $exclusions = @()
    $notManifestBacked = @()
    $discoveryErrors = @()
    $selectedDefinitions = @($RootDefinitions | Where-Object {
        Test-FilterMatch -Value $_.Repository -Patterns $RepositoryPatterns
    })
    $targetIds = @{}

    foreach ($definition in $selectedDefinitions) {
        $repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $ProjectsRoot $definition.Repository))
        $discoveryRoot = [System.IO.Path]::GetFullPath((Join-Path $ProjectsRoot $definition.RelativeRoot))
        $manifestResult = Get-ManifestFiles `
            -Root $discoveryRoot `
            -Repository $definition.Repository `
            -RepositoryRoot $repositoryRoot `
            -RootKind $definition.RootKind
        if ($null -ne $manifestResult.Error) {
            $discoveryErrors += $manifestResult.Error
        }
        $notManifestBacked += @($manifestResult.NotManifestBacked)

        foreach ($manifest in @($manifestResult.Files | Sort-Object FullName)) {
            $metadata = Read-PackageManifest `
                -Manifest $manifest `
                -Repository $definition.Repository `
                -RepositoryRoot $repositoryRoot `
                -BuildTargetTriples $BuildTargetTriples `
                -HostIncompatibleTargetTriples $HostIncompatibleTargetTriples
            if ($metadata.ParseError) {
                $discoveryErrors += [pscustomobject]@{
                    Repository = $definition.Repository
                    Root = $discoveryRoot
                    Type = 'manifest-error'
                    Path = $manifest.FullName
                    Message = $metadata.ParseError
                }
                continue
            }

            if ($metadata.Kind -eq 'Library') {
                $exclusions += [pscustomobject]@{
                    Repository = $metadata.Repository
                    Target = $metadata.TargetName
                    RelativeTargetPath = $metadata.RelativeTargetPath
                    Kind = $metadata.Kind
                    Type = 'non-app-library'
                    Reason = 'package kind is Library'
                    ManifestPath = $metadata.ManifestPath
                }
                continue
            }
            if ($metadata.HostIncompatibleReason) {
                $exclusions += [pscustomobject]@{
                    Repository = $metadata.Repository
                    Target = $metadata.TargetName
                    RelativeTargetPath = $metadata.RelativeTargetPath
                    Kind = $metadata.Kind
                    Type = 'host-incompatible-triple'
                    Triple = $metadata.TargetTriple
                    Reason = $metadata.HostIncompatibleReason
                    ManifestPath = $metadata.ManifestPath
                }
                continue
            }
            if ($metadata.Kind -ne 'App') {
                $discoveryErrors += [pscustomobject]@{
                    Repository = $metadata.Repository
                    Root = $discoveryRoot
                    Type = 'unknown-package-kind'
                    Path = $metadata.ManifestPath
                    Message = "package kind '$($metadata.Kind)' is not App or Library"
                }
                continue
            }

            $targetPath = [System.IO.Path]::GetFullPath($manifest.DirectoryName)
            $targetId = "$($metadata.Repository)/$($metadata.RelativeTargetPath)"
            if ($targetIds.ContainsKey($targetId)) {
                $discoveryErrors += [pscustomobject]@{
                    Repository = $metadata.Repository
                    Root = $discoveryRoot
                    Type = 'duplicate-target'
                    Path = $metadata.ManifestPath
                    Message = "target '$targetId' is also declared by '$($targetIds[$targetId])'"
                }
                continue
            }
            $targetIds[$targetId] = $metadata.ManifestPath

            $targetRecord = [pscustomobject]@{
                Id = $targetId
                Repository = $metadata.Repository
                Target = $metadata.TargetName
                RelativeTargetPath = $metadata.RelativeTargetPath
                PackageName = $metadata.PackageName
                Kind = $metadata.Kind
                BuildTarget = $metadata.BuildTarget
                TargetTriple = $metadata.TargetTriple
                ManifestPath = $metadata.ManifestPath
                WorkingDirectory = $targetPath
            }
            $discoveredTargets += $targetRecord
            if (Test-TargetFilter `
                    -RepositoryName $metadata.Repository `
                    -TargetName $metadata.TargetName `
                    -RelativeTargetPath $metadata.RelativeTargetPath `
                    -Patterns $TargetPatterns) {
                $allTargets += $targetRecord
            }
        }
    }

    return [pscustomobject]@{
        SelectedDefinitions = @($selectedDefinitions)
        DiscoveredTargets = @($discoveredTargets | Sort-Object @{ Expression = { $RepositoryOrder[$_.Repository] } }, RelativeTargetPath, ManifestPath)
        SelectedTargets = @($allTargets | Sort-Object @{ Expression = { $RepositoryOrder[$_.Repository] } }, RelativeTargetPath, ManifestPath)
        Exclusions = @($exclusions | Sort-Object @{ Expression = { $RepositoryOrder[$_.Repository] } }, RelativeTargetPath, Type)
        NotManifestBacked = @($notManifestBacked | Sort-Object @{ Expression = { $RepositoryOrder[$_.Repository] } }, RelativePath)
        DiscoveryErrors = @($discoveryErrors | Sort-Object @{ Expression = { $RepositoryOrder[$_.Repository] } }, Path, Type)
    }
}
