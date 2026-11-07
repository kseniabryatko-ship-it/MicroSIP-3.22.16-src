$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = $PSScriptRoot
$exe = Join-Path $root 'target\release\microsip-rs.exe'
$logDirectory = Join-Path $root 'target\direct-call-test'

if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "Executable not found: $exe. Build the release binary first."
}

New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null

$workers = New-Object System.Collections.ArrayList

function Start-Phone {
    param(
        [string]$Name,
        [string]$Arguments
    )

    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $exe
    $info.Arguments = $Arguments
    $info.WorkingDirectory = $root
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info

    try {
        if (-not $process.Start()) {
            throw "Unable to start $Name."
        }

        # Read both streams concurrently to avoid full-pipe deadlocks.
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()

        return [pscustomobject]@{
            Name = $Name
            Process = $process
            StdoutTask = $stdoutTask
            StderrTask = $stderrTask
        }
    }
    catch {
        try {
            if (-not $process.HasExited) {
                $process.Kill()
                $process.WaitForExit()
            }
        }
        finally {
            $process.Dispose()
        }
        throw
    }
}

function Assert-CallOutput {
    param(
        [string]$Name,
        [string]$Text
    )

    if ([string]::IsNullOrWhiteSpace($Text)) {
        throw "Empty output from $Name."
    }

    if ($Text -notmatch 'Call finished\.') {
        throw "$Name did not finish the call normally."
    }

    $sent = [regex]::Match($Text, 'RTP sent:\s*(\d+)')
    $received = [regex]::Match($Text, 'RTP received:\s*(\d+)')

    if (-not $sent.Success -or -not $received.Success) {
        throw "Missing RTP counters from $Name."
    }

    if ([long]$sent.Groups[1].Value -le 0) {
        throw "$Name sent no RTP packets."
    }

    if ([long]$received.Groups[1].Value -le 0) {
        throw "$Name received no RTP packets."
    }
}

try {
    Write-Host 'Starting the auto-answer client on 127.0.0.1:5070...'
    $listener = Start-Phone `
        -Name 'listener' `
        -Arguments 'listen 127.0.0.1:5070'
    [void]$workers.Add($listener)

    Start-Sleep -Seconds 2

    if ($listener.Process.HasExited) {
        $listener.Process.WaitForExit()
        throw "Listener exited before the call: $($listener.Process.ExitCode)."
    }

    Write-Host 'Starting the caller on 127.0.0.1:5072...'
    $caller = Start-Phone `
        -Name 'caller' `
        -Arguments 'call 127.0.0.1:5072 127.0.0.1:5070 10'
    [void]$workers.Add($caller)

    $deadline = [DateTime]::UtcNow.AddSeconds(60)

    while ($true) {
        $callerExited = $caller.Process.HasExited
        $listenerExited = $listener.Process.HasExited

        if ($callerExited -and $listenerExited) {
            break
        }

        if ([DateTime]::UtcNow -ge $deadline) {
            throw 'The call exceeded the 60-second test deadline.'
        }

        Start-Sleep -Milliseconds 100
    }

    foreach ($worker in $workers) {
        $worker.Process.WaitForExit()

        # ExitCode is read while the directly started Process is alive.
        $exitCode = $worker.Process.ExitCode
        Write-Host "$($worker.Name) exit code: $exitCode"

        if ($exitCode -ne 0) {
            throw "$($worker.Name) failed with exit code $exitCode."
        }

        $stdout = $worker.StdoutTask.GetAwaiter().GetResult()
        Assert-CallOutput -Name $worker.Name -Text $stdout
    }

    Write-Host ''
    Write-Host 'PASS: both processes exited with code 0 and transferred RTP.'
    Write-Host 'This verifies packet transfer, not perceived audio quality.'
}
finally {
    foreach ($worker in $workers) {
        try {
            # Only terminate processes created by this script.
            if (-not $worker.Process.HasExited) {
                $worker.Process.Kill()
            }
            $worker.Process.WaitForExit()

            $stdout = $worker.StdoutTask.GetAwaiter().GetResult()
            $stderr = $worker.StderrTask.GetAwaiter().GetResult()

            $stdoutPath = Join-Path $logDirectory ($worker.Name + '.stdout.txt')
            $stderrPath = Join-Path $logDirectory ($worker.Name + '.stderr.txt')

            Set-Content -LiteralPath $stdoutPath -Value $stdout -Encoding UTF8
            Set-Content -LiteralPath $stderrPath -Value $stderr -Encoding UTF8

            Write-Host "`n===== $($worker.Name) stdout ====="
            Write-Host $stdout
            Write-Host "===== $($worker.Name) stderr ====="
            Write-Host $stderr
        }
        finally {
            $worker.Process.Dispose()
        }
    }
}