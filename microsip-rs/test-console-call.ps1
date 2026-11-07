$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = $PSScriptRoot
$workers = New-Object System.Collections.ArrayList
$previousLocation = Get-Location
$previousDebug = $env:CARGO_PROFILE_RELEASE_DEBUG
$logDirectory = Join-Path $root (
    'target\console-call-tests\' +
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
    & cargo build --locked --release -p microsip-core --bins
    if ($LASTEXITCODE -ne 0) {
        throw 'Release build failed.'
    }

    $metadataLines = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo metadata failed.'
    }
    $metadata = ($metadataLines -join "`n") | ConvertFrom-Json
    $releaseDirectory = Join-Path ([string]$metadata.target_directory) 'release'
    $consoleExe = Join-Path $releaseDirectory 'phone-console.exe'
    $callerExe = Join-Path $releaseDirectory 'microsip-rs.exe'

    foreach ($exe in @($consoleExe, $callerExe)) {
        if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
            throw "Missing executable: $exe"
        }
    }

    $listener = Start-Client `
        -Name 'console' `
        -Executable $consoleExe `
        -Arguments '127.0.0.1:5070'
    Wait-Text -Worker $listener -Pattern 'Commands: answer'

    # Scenario 1: caller ends the accepted call.
    Write-Host '`nTEST 1: answer, then remote hangup'
    $offset = $listener.Output.Length
    $caller1 = Start-Client `
        -Name 'caller-remote-hangup' `
        -Executable $callerExe `
        -Arguments 'call 127.0.0.1:5072 127.0.0.1:5070 5'

    Wait-Text -Worker $listener -Pattern 'Incoming call' -Offset $offset
    Send-Command -Worker $listener -Command 'answer'
    Wait-Text -Worker $listener -Pattern 'Connected\.' -Offset $offset
    Wait-Exit -Worker $caller1 -ExpectedCode 0
    Wait-Text -Worker $listener -Pattern 'State: Idle' -Offset $offset
    Assert-Rtp -Name 'caller 1' -Text $caller1.Output.ToString()
    Assert-Rtp -Name 'console call 1' -Text $listener.Output.ToString().Substring($offset)
    Write-Host 'PASS 1'

    # Scenario 2: console ends the accepted call.
    Write-Host '`nTEST 2: answer, then local hangup'
    $offset = $listener.Output.Length
    $caller2 = Start-Client `
        -Name 'caller-local-hangup' `
        -Executable $callerExe `
        -Arguments 'call 127.0.0.1:5072 127.0.0.1:5070 30'

    Wait-Text -Worker $listener -Pattern 'Incoming call' -Offset $offset
    Send-Command -Worker $listener -Command 'answer'
    Wait-Text -Worker $listener -Pattern 'Connected\.' -Offset $offset
    Start-Sleep -Seconds 3
    Send-Command -Worker $listener -Command 'status'
    Send-Command -Worker $listener -Command 'hangup'
    Wait-Exit -Worker $caller2 -ExpectedCode 0
    Wait-Text -Worker $listener -Pattern 'State: Idle' -Offset $offset
    Assert-Rtp -Name 'caller 2' -Text $caller2.Output.ToString()
    Assert-Rtp -Name 'console call 2' -Text $listener.Output.ToString().Substring($offset)
    Write-Host 'PASS 2'

    # Scenario 3: reject without starting a media session.
    Write-Host '`nTEST 3: reject incoming call'
    $offset = $listener.Output.Length
    $caller3 = Start-Client `
        -Name 'caller-rejected' `
        -Executable $callerExe `
        -Arguments 'call 127.0.0.1:5072 127.0.0.1:5070 10'

    Wait-Text -Worker $listener -Pattern 'Incoming call' -Offset $offset
    Send-Command -Worker $listener -Command 'reject'
    Wait-Text -Worker $listener -Pattern 'Call rejected: 486' -Offset $offset
    Wait-Exit -Worker $caller3 -ExpectedCode 1
    if ($caller3.Output.ToString() -notmatch 'SIP 486') {
        throw 'Rejected caller did not report SIP 486.'
    }
    Write-Host 'PASS 3: rejection observed; non-2xx ACK is not verified.'

    $offset = $listener.Output.Length
    Send-Command -Worker $listener -Command 'status'
    Wait-Text -Worker $listener -Pattern 'State: Idle' -Offset $offset
    Send-Command -Worker $listener -Command 'quit'
    Wait-Exit -Worker $listener -ExpectedCode 0

    Write-Host ''
    Write-Host 'PASS: all three interactive call scenarios completed.'
    Write-Host 'This verifies current behavior, not full SIP compliance or audio quality.'
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