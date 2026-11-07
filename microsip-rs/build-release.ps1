param(
    [string]$OutputDirectory = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = $PSScriptRoot
$previousLocation = Get-Location
$previousDebug = $env:CARGO_PROFILE_RELEASE_DEBUG
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Invoke-Cargo {
    param([string[]]$CargoArguments)

    Write-Host ('> cargo ' + ($CargoArguments -join ' '))
    & cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw ('Cargo command failed: ' + ($CargoArguments -join ' '))
    }
}

function Write-Text {
    param([string]$Path, [string]$Text)

    [System.IO.File]::WriteAllText($Path, $Text, $utf8)
}

function Test-ConsoleHelp {
    param([string]$Executable)

    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $Executable
    $info.Arguments = '--help'
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info

    try {
        if (-not $process.Start()) {
            throw "Unable to start: $Executable"
        }

        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()

        if (-not $process.WaitForExit(15000)) {
            $process.Kill()
            $process.WaitForExit()
            throw "Help command timed out: $Executable"
        }

        $text = $stdout.GetAwaiter().GetResult()
        $errorText = $stderr.GetAwaiter().GetResult()
        $code = $process.ExitCode

        if ($code -ne 0) {
            throw "Help command failed ($code): $Executable`n$errorText"
        }
        if ([string]::IsNullOrWhiteSpace($text)) {
            throw "Help command produced no output: $Executable"
        }
        Write-Host ('Help check passed: ' + [IO.Path]::GetFileName($Executable))
    }
    finally {
        try {
            if ($process.Id -gt 0 -and -not $process.HasExited) {
                $process.Kill()
                $process.WaitForExit()
            }
        }
        finally {
            $process.Dispose()
        }
    }
}

try {
    Set-Location -LiteralPath $root

    if (-not (Test-Path -LiteralPath 'Cargo.lock' -PathType Leaf)) {
        throw 'Cargo.lock is required.'
    }

    $rustInfo = & rustc -vV
    if ($LASTEXITCODE -ne 0) {
        throw 'rustc -vV failed.'
    }
    if (-not ($rustInfo -contains 'host: x86_64-pc-windows-msvc')) {
        throw 'This package builder requires Windows x64 MSVC Rust.'
    }

    Invoke-Cargo -CargoArguments @('fmt', '--all', '--', '--check')
    Invoke-Cargo -CargoArguments @('test', '--locked', '--workspace')
    Invoke-Cargo -CargoArguments @(
        'clippy', '--locked', '--workspace', '--all-targets', '--', '-D', 'warnings'
    )

    $env:CARGO_PROFILE_RELEASE_DEBUG = '0'
    Invoke-Cargo -CargoArguments @(
        'build', '--locked', '--release', '-p', 'microsip-core', '--bins'
    )

    $metadataOutput = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo metadata failed.'
    }
    $metadata = ($metadataOutput -join "`n") | ConvertFrom-Json
    $target = [string]$metadata.target_directory
    $release = Join-Path $target 'release'

    $binaries = @(
        'microsip-gui.exe',
        'microsip-rs.exe',
        'phone-console.exe'
    )
    foreach ($name in $binaries) {
        if (-not (Test-Path -LiteralPath (Join-Path $release $name) -PathType Leaf)) {
            throw "Missing release binary: $name"
        }
    }

    if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
        $OutputDirectory = Join-Path $target 'portable-releases'
    }
    $output = [IO.Path]::GetFullPath($OutputDirectory)
    New-Item -ItemType Directory -Path $output -Force | Out-Null

    $version = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss')
    $packageName = 'microsip-rs-gui-preview-' + $version
    $package = Join-Path $output $packageName
    if (Test-Path -LiteralPath $package) {
        throw "Package directory already exists: $package"
    }
    New-Item -ItemType Directory -Path $package | Out-Null

    foreach ($name in $binaries) {
        Copy-Item -LiteralPath (Join-Path $release $name) -Destination $package
    }

    Test-ConsoleHelp -Executable (Join-Path $package 'microsip-rs.exe')
    Test-ConsoleHelp -Executable (Join-Path $package 'phone-console.exe')

    $readme = @'
MICROSIP RUST - GUI DEVELOPMENT PREVIEW
======================================

START
-----
Run microsip-gui.exe.
Keep all three EXE files in the SAME directory.
This preview uses the two console programs as child processes.

LOCAL TEST WITH TWO WINDOWS
---------------------------
Window A:
  Local IP:port = 127.0.0.1:5070
  Click Listen for incoming calls.

Window B:
  Local IP:port = 127.0.0.1:5072
  Remote IP:port = 127.0.0.1:5070
  Choose call duration, then click Call.

Window A:
  Click Answer when Incoming call appears.
  Use headphones and speak into the microphone.
  Click Hang up, wait for Idle, then click Quit listener.

Outgoing calls currently finish after the selected duration
or after a remote BYE. There is no GUI outgoing CANCEL yet.

IMPORTANT
---------
Do not close the GUI during an active call. Closing the window currently
forces child-process cleanup, not a graceful SIP hangup.

FEATURES VERIFIED SEPARATELY BY LOCAL CALL TESTS
-----------------------------------------------
Direct UDP calls, manual incoming answer/rejection, PCMU/RTP audio,
call termination and repeated incoming calls.

LIMITATIONS
-----------
No provider registration in this GUI.
No guarantee of NAT traversal or interoperability with third-party phones.
No TLS/SRTP, installer, code signature or automatic updates.
No claim of complete CANCEL or non-2xx ACK network handling.
A successful build does not prove audio quality or a working real call.
Do not expose the listener directly to an untrusted network.

SYSTEM
------
Windows x64. Microsoft Visual C++ Runtime x64 may be required.
Microphone and speaker access are required for calls.

DISTRIBUTION
------------
Development test package, not an audited public release.
Complete the source-license and dependency-notice review before public
redistribution. No credentials should be stored in this directory.
'@
    Write-Text -Path (Join-Path $package 'README.txt') -Text $readme

    $commit = 'unknown'
    $workingTree = 'unknown'
    if ($null -ne (Get-Command git -ErrorAction SilentlyContinue)) {
        $commitOutput = & git rev-parse HEAD
        if ($LASTEXITCODE -eq 0) {
            $commit = ($commitOutput -join '').Trim()
        }
        $statusOutput = @(& git status --porcelain)
        if ($LASTEXITCODE -eq 0) {
            $workingTree = if ($statusOutput.Count -eq 0) { 'clean' } else { 'modified or untracked files' }
        }
    }

    $manifest = [ordered]@{
        package = $packageName
        built_utc = [DateTime]::UtcNow.ToString('o')
        commit = $commit
        working_tree = $workingTree
        target = 'x86_64-pc-windows-msvc'
        rust = ($rustInfo -join "`n")
        binaries = $binaries
        runtime_call_tested_by_this_script = $false
    }
    Write-Text -Path (Join-Path $package 'BUILD-INFO.json') `
        -Text ($manifest | ConvertTo-Json -Depth 4)

    $checksums = foreach ($name in $binaries) {
        $hash = (Get-FileHash -LiteralPath (Join-Path $package $name) -Algorithm SHA256).Hash
        $hash + '  ' + $name
    }
    Write-Text -Path (Join-Path $package 'SHA256SUMS.txt') `
        -Text (($checksums -join "`r`n") + "`r`n")

    $archive = Join-Path $output ($packageName + '.zip')
    Compress-Archive -Path $package -DestinationPath $archive
    $archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    [IO.File]::WriteAllText(
        $archive + '.sha256',
        $archiveHash + '  ' + [IO.Path]::GetFileName($archive) + "`r`n",
        [Text.Encoding]::ASCII
    )

    Write-Host ''
    Write-Host 'PASS: checks, release build and GUI package creation.'
    Write-Host "Folder: $package"
    Write-Host "ZIP: $archive"
    Write-Host "Archive SHA256: $archiveHash"
    Write-Host 'Manual GUI call testing is still required.'
}
finally {
    $env:CARGO_PROFILE_RELEASE_DEBUG = $previousDebug
    Set-Location $previousLocation
}