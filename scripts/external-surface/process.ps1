function Test-FirstFrameLine {
    param(
        [AllowEmptyString()]
        [string]$Line
    )

    return $Line -match '^\s*@kira\.live\s+live\.first_frame(?:\s|$)'
}

function Receive-CompletedLines {
    param(
        [Parameter(Mandatory = $true)]
        [ref]$ReadTask,
        [Parameter(Mandatory = $true)]
        [ref]$Done,
        [Parameter(Mandatory = $true)]
        [System.IO.TextReader]$Reader,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[string]]$Lines,
        [Parameter(Mandatory = $true)]
        [ref]$FirstFrame,
        [Parameter(Mandatory = $true)]
        [ref]$ReadError
    )

    while (-not $Done.Value -and $null -ne $ReadTask.Value -and $ReadTask.Value.IsCompleted) {
        try {
            $line = $ReadTask.Value.GetAwaiter().GetResult()
            if ($null -eq $line) {
                $Done.Value = $true
                break
            }
            [void]$Lines.Add($line)
            if (Test-FirstFrameLine -Line $line) {
                $FirstFrame.Value = $true
            }
            $ReadTask.Value = $Reader.ReadLineAsync()
        }
        catch {
            $Done.Value = $true
            $ReadError.Value = $_.Exception.Message
        }
    }
}

function Stop-OwnProcessTree {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process
    )

    if ($Process.HasExited) {
        return
    }

    $processId = $Process.Id
    $killError = $null
    $taskKillOutput = @()
    $taskKillExitCode = $null
    try {
        $taskKillOutput = @(& taskkill.exe /PID ([string]$processId) /T /F 2>&1)
        $taskKillExitCode = $LASTEXITCODE
        if ($taskKillExitCode -ne 0) {
            $detail = ($taskKillOutput -join ' ').Trim()
            if ([string]::IsNullOrEmpty($detail)) {
                $detail = 'taskkill.exe returned no diagnostic'
            }
            $killError = "taskkill.exe exited with code ${taskKillExitCode}: $detail"
        }
    }
    catch {
        $killError = $_.Exception.Message
    }

    try {
        if (-not $Process.HasExited) {
            [void]$Process.WaitForExit(5000)
        }
    }
    catch {
        $waitError = $_.Exception.Message
        $killError = if ($killError) { "$killError; $waitError" } else { $waitError }
    }

    if (-not $Process.HasExited) {
        try {
            $Process.Kill($true)
            if (-not $Process.WaitForExit(5000)) {
                throw "process $processId was still running after Process.Kill"
            }
            $killError = $null
        }
        catch {
            $fallbackError = $_.Exception.Message
            $killError = if ($killError) { "$killError; $fallbackError" } else { $fallbackError }
        }
    }

    if (-not $Process.HasExited) {
        $remaining = "process $processId was still running after tree termination"
        $killError = if ($killError) { "$killError; $remaining" } else { $remaining }
    }

    if ($killError) {
        throw "could not terminate process tree for PID ${processId}: $killError"
    }
}

function Get-DiagnosticSignals {
    param(
        [AllowEmptyString()]
        [string]$Output
    )

    $signals = @()
    $patterns = [ordered]@{
        'cli-rejection' = '(?im)^\s*kira:\s+(?:unknown backend|unknown option|unknown command|expects one of|expected one of|usage:)'
        'compiler-error' = '(?im)^\s*(?:error(?:\[[^\]]+\])?:|kira:\s+(?:error|failed|failure|could not|cannot|unknown|unsupported|not supported|access denied|permission denied))'
    }
    foreach ($name in $patterns.Keys) {
        if ($Output -match $patterns[$name]) {
            $signals += $name
        }
    }
    return @($signals)
}

function Test-TargetFiniteOutput {
    param(
        [AllowEmptyString()]
        [string]$Stdout
    )

    foreach ($line in @($Stdout -split "`r?`n")) {
        $trimmed = $line.Trim()
        if ([string]::IsNullOrEmpty($trimmed)) {
            continue
        }
        if ($trimmed -match '(?i)^@kira\.live\b' -or $trimmed -match '(?i)^kira(?:\s|:|$)') {
            continue
        }
        return $true
    }

    return $false
}

function Get-FirstDiagnosticLine {
    param(
        [AllowEmptyString()]
        [string]$Output
    )

    $lines = @($Output -split "`r?`n" | ForEach-Object { $_.Trim() } | Where-Object { $_.Length -gt 0 })
    if ($lines.Count -eq 0) {
        return $null
    }
    $diagnostic = $lines | Where-Object {
        $_ -match '(?i)(^kira:\s|^error(?:\[[^\]]+\])?:|\berror\b|\bfailed\b|\bfailure\b|\bunknown\b|\bunsupported\b|\bcannot\b|access denied|permission denied|usage:)'
    } | Select-Object -First 1
    if ($null -ne $diagnostic) {
        return [string]$diagnostic
    }
    return [string]$lines[0]
}

function New-CellResult {
    param(
        [Parameter(Mandatory = $true)]
        [pscustomobject]$Cell
    )

    $result = [ordered]@{}
    foreach ($property in $Cell.PSObject.Properties) {
        $result[$property.Name] = $property.Value
    }
    return [pscustomobject]$result
}

function Invoke-MatrixCell {
    param(
        [Parameter(Mandatory = $true)]
        [pscustomobject]$Cell,
        [Parameter(Mandatory = $true)]
        [string]$KiraPath,
        [Parameter(Mandatory = $true)]
        [ValidateRange(1, 86400000)]
        [int]$TimeoutMilliseconds
    )

    $result = New-CellResult -Cell $Cell
    $stdoutLines = New-Object 'System.Collections.Generic.List[string]'
    $stderrLines = New-Object 'System.Collections.Generic.List[string]'
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $process = $null
    $processStarted = $false
    $spawnError = $null
    $readError = $null
    $killError = $null
    $processId = $null
    $timedOut = $false
    $termination = 'natural'
    $terminatedAfterFirstFrame = $false
    $firstFrame = $false
    $stdoutDone = $false
    $stderrDone = $false
    $stdoutTask = $null
    $stderrTask = $null

    try {
        $startInfo = New-Object System.Diagnostics.ProcessStartInfo
        $startInfo.FileName = $KiraPath
        $startInfo.WorkingDirectory = $Cell.WorkingDirectory
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        foreach ($argument in $Cell.Arguments) {
            [void]$startInfo.ArgumentList.Add([string]$argument)
        }

        $process = New-Object System.Diagnostics.Process
        $process.StartInfo = $startInfo
        $processStarted = $process.Start()
        if (-not $processStarted) {
            throw 'Process.Start returned false'
        }
        $processId = $process.Id
        $stdoutTask = $process.StandardOutput.ReadLineAsync()
        $stderrTask = $process.StandardError.ReadLineAsync()
        $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)

        while ($true) {
            Receive-CompletedLines `
                -ReadTask ([ref]$stdoutTask) -Done ([ref]$stdoutDone) `
                -Reader $process.StandardOutput -Lines $stdoutLines `
                -FirstFrame ([ref]$firstFrame) -ReadError ([ref]$readError)
            Receive-CompletedLines `
                -ReadTask ([ref]$stderrTask) -Done ([ref]$stderrDone) `
                -Reader $process.StandardError -Lines $stderrLines `
                -FirstFrame ([ref]$firstFrame) -ReadError ([ref]$readError)

            if ($Cell.Mode -eq 'run' -and $firstFrame -and -not $process.HasExited) {
                $termination = 'first-frame'
                $terminatedAfterFirstFrame = $true
                Stop-OwnProcessTree -Process $process
                break
            }

            if ($process.HasExited -and $stdoutDone -and $stderrDone) {
                break
            }

            if ([DateTime]::UtcNow -ge $deadline) {
                $timedOut = $true
                $termination = 'timeout'
                if (-not $process.HasExited) {
                    Stop-OwnProcessTree -Process $process
                }
                break
            }

            [System.Threading.Thread]::Sleep(25)
        }

        if (-not $process.HasExited) {
            [void]$process.WaitForExit(5000)
        }

        $drainDeadline = [DateTime]::UtcNow.AddMilliseconds(2000)
        while ((-not $stdoutDone -or -not $stderrDone) -and [DateTime]::UtcNow -lt $drainDeadline) {
            Receive-CompletedLines `
                -ReadTask ([ref]$stdoutTask) -Done ([ref]$stdoutDone) `
                -Reader $process.StandardOutput -Lines $stdoutLines `
                -FirstFrame ([ref]$firstFrame) -ReadError ([ref]$readError)
            Receive-CompletedLines `
                -ReadTask ([ref]$stderrTask) -Done ([ref]$stderrDone) `
                -Reader $process.StandardError -Lines $stderrLines `
                -FirstFrame ([ref]$firstFrame) -ReadError ([ref]$readError)
            if (-not $stdoutDone -or -not $stderrDone) {
                [System.Threading.Thread]::Sleep(25)
            }
        }
    }
    catch {
        if (-not $processStarted) {
            $spawnError = $_.Exception.Message
        }
        else {
            $readError = $_.Exception.Message
        }
    }
    finally {
        $stopwatch.Stop()
        if ($null -ne $process -and $processStarted) {
            try {
                if (-not $process.HasExited) {
                    Stop-OwnProcessTree -Process $process
                }
            }
            catch {
                $killError = if ($killError) { "$killError; $($_.Exception.Message)" } else { $_.Exception.Message }
            }
        }
    }

    $stdout = [string]::Join([Environment]::NewLine, $stdoutLines.ToArray())
    $stderr = [string]::Join([Environment]::NewLine, $stderrLines.ToArray())
    $signals = @(Get-DiagnosticSignals -Output $stderr)
    $finiteOutput = Test-TargetFiniteOutput -Stdout $stdout
    $exitCode = $null
    if ($null -ne $process -and $processStarted) {
        try {
            if ($process.HasExited) {
                $exitCode = $process.ExitCode
            }
        }
        catch {
            $readError = if ($readError) { "$readError; $($_.Exception.Message)" } else { $_.Exception.Message }
        }
    }

    $result.ProcessId = $processId
    $result.ExitCode = $exitCode
    $result.DurationMs = [math]::Round($stopwatch.Elapsed.TotalMilliseconds, 1)
    $result.TimedOut = $timedOut
    $result.Termination = $termination
    $result.FirstFrame = $firstFrame
    $result.FiniteOutput = $finiteOutput
    $result.DiagnosticSignals = @($signals)
    $result.CaptureError = if ($readError) { $readError } else { $killError }
    $result.Stdout = $stdout
    $result.Stderr = $stderr

    $diagnosticOutput = if (-not [string]::IsNullOrWhiteSpace($stderr)) { $stderr } else { $stdout }
    $failureReason = Get-FirstDiagnosticLine -Output $diagnosticOutput
    if ($spawnError) {
        $result.Status = 'spawn-failed'
        $result.Success = $false
        $result.Classification = 'process-start'
        $result.FailureReason = $spawnError
    }
    elseif ($killError) {
        $result.Status = 'cleanup-failed'
        $result.Success = $false
        $result.Classification = 'process-tree-stop'
        $result.FailureReason = $killError
    }
    elseif ($readError) {
        $result.Status = 'capture-failed'
        $result.Success = $false
        $result.Classification = 'output-capture'
        $result.FailureReason = $readError
    }
    elseif ($null -ne $exitCode -and $exitCode -ne 0 -and -not $timedOut -and -not ($terminatedAfterFirstFrame -and $firstFrame)) {
        $result.Status = if ($signals -contains 'cli-rejection') { 'cli-rejection' } else { 'failed' }
        $result.Success = $false
        $result.Classification = if ($signals.Count -gt 0) { 'diagnostic' } else { 'nonzero-exit' }
        $result.FailureReason = if ($failureReason) { $failureReason } else { "child exited with code $exitCode" }
    }
    elseif ($signals.Count -gt 0) {
        $result.Status = 'diagnostic-error'
        $result.Success = $false
        $result.Classification = 'fatal-diagnostic'
        $result.FailureReason = if ($failureReason) { $failureReason } else { 'compiler diagnostic was emitted' }
    }
    elseif ($timedOut) {
        $result.Status = 'timeout'
        $result.Success = $false
        $result.Classification = 'no-bounded-completion'
        $result.FailureReason = if ($failureReason) { $failureReason } else { 'child process exceeded the timeout' }
    }
    elseif ($terminatedAfterFirstFrame -and $firstFrame) {
        $result.Status = 'passed'
        $result.Success = $true
        $result.Classification = 'first-frame-marker'
        $result.FailureReason = $null
    }
    elseif ($firstFrame) {
        $result.Status = 'passed'
        $result.Success = $true
        $result.Classification = 'first-frame-marker'
        $result.FailureReason = $null
    }
    elseif ($finiteOutput -and $null -ne $exitCode -and $exitCode -eq 0) {
        $result.Status = 'passed'
        $result.Success = $true
        $result.Classification = 'finite-output'
        $result.FailureReason = $null
    }
    else {
        $result.Status = 'failed'
        $result.Success = $false
        $result.Classification = 'no-success-evidence'
        $result.FailureReason = if ($failureReason) { $failureReason } else { 'exit 0 without a first-frame marker or target output' }
    }

    if ($null -ne $process) {
        $process.Dispose()
    }
    return $result
}
