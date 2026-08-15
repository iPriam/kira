function Resolve-JsonPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $Path))
}

function Write-JsonReport {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Report,
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $parent = Split-Path -Parent $Path
    if ($parent) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
    $json = $Report | ConvertTo-Json -Depth 12
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $json, $utf8)
}

function Format-FailureReason {
    param(
        [AllowEmptyString()]
        [string]$Reason
    )

    if ([string]::IsNullOrEmpty($Reason)) {
        return '-'
    }
    $oneLine = ($Reason -replace '\s+', ' ').Trim()
    if ($oneLine.Length -gt 160) {
        return $oneLine.Substring(0, 157) + '...'
    }
    return $oneLine
}

function Write-FailureTable {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Results,
        [Parameter(Mandatory = $true)]
        [ValidateRange(1, 1000)]
        [int]$Limit
    )

    $failures = @($Results | Where-Object { $_.Success -ne $true })
    Write-Output "Failures: $($failures.Count)"
    Write-Output 'Failure table:'
    if ($failures.Count -eq 0) {
        Write-Output '  none'
        return
    }

    Write-Output '  repository | target | mode | backend | status | exit | reason'
    $shown = 0
    foreach ($failure in $failures) {
        if ($shown -ge $Limit) {
            break
        }
        $exit = if ($null -eq $failure.ExitCode) { '-' } else { [string]$failure.ExitCode }
        Write-Output ('  {0} | {1} | {2} | {3} | {4} | {5} | {6}' -f `
            $failure.Repository, $failure.Target, $failure.Mode, $failure.Backend,
            $failure.Status, $exit, (Format-FailureReason -Reason $failure.FailureReason))
        $shown++
    }
    if ($failures.Count -gt $shown) {
        Write-Output "  ... $($failures.Count - $shown) more; see JSON for complete output"
    }
}
