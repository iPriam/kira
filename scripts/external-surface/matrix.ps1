function New-Cell {
    param(
        [Parameter(Mandatory = $true)]
        [pscustomobject]$TargetRecord,
        [Parameter(Mandatory = $true)]
        [string]$CellMode,
        [Parameter(Mandatory = $true)]
        [string]$CellBackend,
        [Parameter(Mandatory = $true)]
        [string]$QuitAfter
    )

    if ($CellMode -eq 'run') {
        $argumentList = @('run', '.', '--backend', $CellBackend)
    }
    else {
        $argumentList = @('live', '.', '--no-watch', '--quit-after', $QuitAfter, '--backend', $CellBackend)
    }
    $argumentLine = $argumentList -join ' '

    return [pscustomobject][ordered]@{
        Id = "$($TargetRecord.Id)|$CellMode|$CellBackend"
        Repository = $TargetRecord.Repository
        Target = $TargetRecord.Target
        RelativeTargetPath = $TargetRecord.RelativeTargetPath
        PackageName = $TargetRecord.PackageName
        ManifestPath = $TargetRecord.ManifestPath
        WorkingDirectory = $TargetRecord.WorkingDirectory
        Mode = $CellMode
        Backend = $CellBackend
        Arguments = @($argumentList)
        ArgumentLine = $argumentLine
        Command = "kira $argumentLine"
        Status = 'planned'
        Success = $null
        Classification = $null
        ExitCode = $null
        ProcessId = $null
        DurationMs = $null
        TimedOut = $false
        Termination = 'not-started'
        FirstFrame = $false
        FiniteOutput = $false
        DiagnosticSignals = @()
        FailureReason = $null
        CaptureError = $null
        Stdout = ''
        Stderr = ''
    }
}

function New-UnavailableCellResult {
    param(
        [Parameter(Mandatory = $true)]
        [pscustomobject]$Cell,
        [Parameter(Mandatory = $true)]
        [string]$Reason
    )

    $result = New-CellResult -Cell $Cell
    $result.Status = 'spawn-failed'
    $result.Success = $false
    $result.Classification = 'kira-path'
    $result.FailureReason = $Reason
    $result.DurationMs = 0
    return $result
}
