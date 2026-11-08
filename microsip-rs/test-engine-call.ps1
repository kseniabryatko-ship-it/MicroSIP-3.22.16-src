$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = $PSScriptRoot
$workers = New-Object System.Collections.ArrayList
$previousLocation = Get-Location
$previousDebug = $env:CARGO_PROFILE_RELEASE_DEBUG
$logDirectory = Join-Path $root (
    'target\engine-call-tests\' +
    [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss')
)

function Start-Client {
    param(
        [string]$Name,
        [string]$Executable,
        [string]$Arguments
    )

    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $Executable
    $info.Arguments = $Arguments
    $info.WorkingDirectory = $root
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info

    if (-not $process.Start()) {
        $process.Dispose()
        throw "Unable to start $Name."
    }

    $worker = [pscustomobject]@{
        Name = $Name
        Process = $process
        Output = New-Object System.Text.StringBuilder
        NextLine = $process.StandardOutput.ReadLineAsync()
        OutputClosed = $false
        ErrorTask = $process.StandardError.ReadToEndAsync()
        Saved = $false
    }

    $process.StandardInput.AutoFlush = $true
    [void]$workers.Add($worker)
    return $worker
}

function Pump-Output {
    param($Worker)

    while (-not $Worker.OutputClosed -and $Worker.NextLine.IsCompleted) {
        $line = $Worker.NextLine.GetAwaiter().GetResult()
        if ($null -eq $line) {
            $Worker.OutputClosed = $true
            break
        }
        [void]$Worker.Output.AppendLine($line)
        Write-Host ("[{0}] {1}" -f $Worker.Name, $line)
        $Worker.NextLine = $Worker.Process.StandardOutput.ReadLineAsync()
    }
}

function Pump-All {
    foreach ($worker in $workers) {
        Pump-Output -Worker $worker
    }
}

function Wait-Text {
    param(
        $Worker,
        [string]$Pattern,
        [int]$Offset = 0,
        [int]$TimeoutSeconds = 20
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        Pump-All
        $text = $Worker.Output.ToString()
        if ($text.Length -ge $Offset -and
            $text.Substring($Offset) -match $Pattern) {
            return
        }
        if ($Worker.Process.HasExited) {
            Pump-All
            throw "$($Worker.Name) exited while waiting for: $Pattern"
        }
        Start-Sleep -Milliseconds 20
    }
    throw "Timeout waiting for $($Worker.Name): $Pattern"
}

function Send-Command {
    param($Worker, [string]$Command)

    if ($Worker.Process.HasExited) {
        throw "Cannot send '$Command': $($Worker.Name) has exited."
    }
    Write-Host ("> {0}: {1}" -f $Worker.Name, $Command)
    $Worker.Process.StandardInput.WriteLine($Command)
}

function Wait-Exit {
    param(
        $Worker,
        [int]$ExpectedCode,
        [int]$TimeoutSeconds = 40
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not $Worker.Process.HasExited) {
        Pump-All
        if ([DateTime]::UtcNow -ge $deadline) {
            throw "Timeout waiting for $($Worker.Name) to exit."
        }
        Start-Sleep -Milliseconds 20
    }

    $Worker.Process.WaitForExit()
    while (-not $Worker.OutputClosed) {
        Pump-All
        if ([DateTime]::UtcNow -ge $deadline) {
            throw "Timeout reading final output from $($Worker.Name)."
        }
        Start-Sleep -Milliseconds 10
    }

    $actual = $Worker.Process.ExitCode
    if ($actual -ne $ExpectedCode) {
        throw "$($Worker.Name): expected exit $ExpectedCode, received $actual."
    }
}

function Assert-Rtp {
    param([string]$Name, [string]$Text)

    $sent = [regex]::Match($Text, 'RTP sent:\s*(\d+)')
    $received = [regex]::Match($Text, '(?:RTP received:|received:)\s*(\d+)')
    if (-not $sent.Success -or -not $received.Success) {
        throw "Missing RTP counters: $Name"
    }
    if ([long]$sent.Groups[1].Value -le 0 -or
        [long]$received.Groups[1].Value -le 0) {
        throw "RTP must flow in both directions: $Name"
    }
}

function Save-Worker {
    param($Worker)

    if ($Worker.Saved) {
        return
    }
    if (-not $Worker.Process.HasExited) {
        $Worker.Process.Kill()
    }
    $Worker.Process.WaitForExit()

    while (-not $Worker.OutputClosed) {
        Pump-Output -Worker $Worker
        if (-not $Worker.OutputClosed) {
            Start-Sleep -Milliseconds 10
        }
    }

    $errors = $Worker.ErrorTask.GetAwaiter().GetResult()
    Set-Content -LiteralPath (
        Join-Path $logDirectory ($Worker.Name + '.stdout.txt')
    ) -Value $Worker.Output.ToString() -Encoding UTF8
    Set-Content -LiteralPath (
        Join-Path $logDirectory ($Worker.Name + '.stderr.txt')
    ) -Value $errors -Encoding UTF8

    if (-not [string]::IsNullOrWhiteSpace($errors)) {
        Write-Host "===== $($Worker.Name) stderr ====="
        Write-Host $errors
    }
    $Worker.Saved = $true
}

try {
    Set-Location -LiteralPath $root
    New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null

    $env:CARGO_PROFILE_RELEASE_DEBUG = '0'
    & cargo build --locked --release -p microsip-core --bin phone-engine
    if ($LASTEXITCODE -ne 0) {
        throw 'Release build failed.'
    }

    $metadataLines = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo metadata failed.'
    }
    $metadata = ($metadataLines -join "`n") | ConvertFrom-Json
    $releaseDirectory = Join-Path ([string]$metadata.target_directory) 'release'
    $engineExe = Join-Path $releaseDirectory 'phone-engine.exe'
    if (-not (Test-Path -LiteralPath $engineExe -PathType Leaf)) {
        throw "Missing executable: $engineExe"
    }

    $alice = Start-Client -Name 'alice' -Executable $engineExe `
        -Arguments '127.0.0.1:5072'
    $bob = Start-Client -Name 'bob' -Executable $engineExe `
        -Arguments '127.0.0.1:5070'
    Wait-Text -Worker $alice -Pattern 'Commands: call'
    Wait-Text -Worker $bob -Pattern 'Commands: call'

    # Scenario 1: the caller ends the accepted call.
    Write-Host "`nTEST 1: answer, then caller hangup"
    $a = $alice.Output.Length
    $b = $bob.Output.Length
    Send-Command -Worker $alice -Command 'call 127.0.0.1:5070'
    Wait-Text -Worker $bob -Pattern 'Incoming call' -Offset $b
    Send-Command -Worker $bob -Command 'answer'
    Wait-Text -Worker $alice -Pattern 'Connected\.' -Offset $a
    Wait-Text -Worker $bob -Pattern 'Connected\.' -Offset $b
    Start-Sleep -Seconds 3
    Send-Command -Worker $alice -Command 'hangup'
    Wait-Text -Worker $alice -Pattern 'Call ended: completed' -Offset $a
    Wait-Text -Worker $bob -Pattern 'Call ended: completed' -Offset $b
    Assert-Rtp -Name 'alice 1' -Text $alice.Output.ToString().Substring($a)
    Assert-Rtp -Name 'bob 1' -Text $bob.Output.ToString().Substring($b)
    Write-Host 'PASS 1'

    # Scenario 2: the called side ends the accepted call.
    Write-Host "`nTEST 2: answer, then callee hangup"
    $a = $alice.Output.Length
    $b = $bob.Output.Length
    Send-Command -Worker $alice -Command 'call 127.0.0.1:5070'
    Wait-Text -Worker $bob -Pattern 'Incoming call' -Offset $b
    Send-Command -Worker $bob -Command 'answer'
    Wait-Text -Worker $alice -Pattern 'Connected\.' -Offset $a
    Wait-Text -Worker $bob -Pattern 'Connected\.' -Offset $b
    Start-Sleep -Seconds 3
    Send-Command -Worker $bob -Command 'hangup'
    Wait-Text -Worker $bob -Pattern 'Call ended: completed' -Offset $b
    Wait-Text -Worker $alice -Pattern 'Call ended: completed' -Offset $a
    Assert-Rtp -Name 'alice 2' -Text $alice.Output.ToString().Substring($a)
    Assert-Rtp -Name 'bob 2' -Text $bob.Output.ToString().Substring($b)
    Write-Host 'PASS 2'

    # Scenario 3: rejection, no media.
    Write-Host "`nTEST 3: reject incoming call"
    $a = $alice.Output.Length
    $b = $bob.Output.Length
    Send-Command -Worker $alice -Command 'call 127.0.0.1:5070'
    Wait-Text -Worker $bob -Pattern 'Incoming call' -Offset $b
    Send-Command -Worker $bob -Command 'reject'
    Wait-Text -Worker $alice -Pattern 'Call ended: rejected \(486\)' -Offset $a
    Wait-Text -Worker $bob -Pattern 'Call ended: rejected \(486\)' -Offset $b
    if ($alice.Output.ToString().Substring($a) -match 'RTP sent') {
        throw 'A rejected call must not start media.'
    }
    Write-Host 'PASS 3'

    # Scenario 4: the caller cancels while the phone is ringing.
    Write-Host "`nTEST 4: cancel before answer"
    $a = $alice.Output.Length
    $b = $bob.Output.Length
    Send-Command -Worker $alice -Command 'call 127.0.0.1:5070'
    Wait-Text -Worker $bob -Pattern 'Incoming call' -Offset $b
    Wait-Text -Worker $alice -Pattern 'Remote is ringing' -Offset $a
    Send-Command -Worker $alice -Command 'hangup'
    Wait-Text -Worker $alice -Pattern 'Call ended: cancelled' -Offset $a
    Wait-Text -Worker $bob -Pattern 'Call ended: cancelled' -Offset $b
    Write-Host 'PASS 4'

    foreach ($worker in @($alice, $bob)) {
        $offset = $worker.Output.Length
        Send-Command -Worker $worker -Command 'status'
        Wait-Text -Worker $worker -Pattern 'State: Idle' -Offset $offset
        Send-Command -Worker $worker -Command 'quit'
        Wait-Exit -Worker $worker -ExpectedCode 0
    }

    Write-Host ''
    Write-Host 'PASS: four call scenarios completed with phone-engine.exe.'
    Write-Host 'This verifies the new engine and runner with real audio devices on this PC,'
    Write-Host 'not provider registration, NAT traversal or audio quality.'
}
finally {
    foreach ($worker in $workers) {
        try {
            Save-Worker -Worker $worker
        }
        catch {
            Write-Warning ("Unable to save {0}: {1}" -f $worker.Name, $_)
        }
        finally {
            $worker.Process.Dispose()
        }
    }
    $env:CARGO_PROFILE_RELEASE_DEBUG = $previousDebug
    Set-Location $previousLocation
    Write-Host "Logs: $logDirectory"
}