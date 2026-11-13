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
        'phone-engine.exe'
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

    Test-ConsoleHelp -Executable (Join-Path $package 'phone-engine.exe')

    $readme = @'
MICROSIP RUST - GUI DEVELOPMENT PREVIEW
======================================

CONTENTS
--------
microsip-gui.exe   - the phone with a window. It needs no other EXE.
phone-engine.exe   - the same phone engine as a console program,
                     for diagnostics and scripted tests.

START
-----
Run microsip-gui.exe. The window is in Russian.

FIRST START
-----------
The settings window opens by itself. Fill in the first three fields:
  1. SIP server   the server address from your provider, with the port
                  if it is not 5060 (host:port)
  2. Number       your extension or account name
  3. Password
Leave the other fields empty unless the provider says otherwise, then
click the left button under the fields (save and connect). The top line
of the window turns green when the phone is registered. At later starts
the phone connects by itself.

The password is remembered only while the "remember password" box is
ticked, and it is stored encrypted for the current Windows user: the
settings file is useless on another computer or Windows account.

USING THE PHONE
---------------
First tab   dial pad: type or click a number and press the green button.
            During a call the keypad sends tones, and there are buttons
            for the microphone and for hold.
Second tab  the list of calls, missed ones marked in red, with redial.
Third tab   contacts: names for numbers; a saved name is shown for calls.
A call can be recorded with the button under "hold", or every call
automatically (settings window). Recordings are WAV files in
Documents\MicroSIP-Rust Recordings: left channel you, right channel the
other side. Tell the other side when you record: the law may require it.
An incoming call rings on the speaker and brings the window forward.
Closing the window ends a call properly and takes the phone offline.

Settings, call list, contacts and the SIP trace are kept in
%APPDATA%\MicroSIP-Rust.

CONSOLE PHONE
-------------
phone-engine.exe LOCAL_IP:SIP_PORT [--no-audio]
Port 0 picks a free port; the chosen address is printed first.
Commands: call IP:PORT, call NUMBER, answer, reject, hangup, status,
quit, register SERVER[:PORT] USER PASSWORD [DOMAIN], unregister.

FEATURES VERIFIED SEPARATELY BY LOCAL CALL TESTS
-----------------------------------------------
Direct UDP calls with one call at a time, incoming answer and
rejection, cancel before answer, hangup from either side,
PCMU/RTP audio in both directions.

LIMITATIONS
-----------
UDP only; one call at a time; one account. Codecs: G.711 (PCMU, PCMA).
Behind a router the phone registers the public address the server
reports and sends keep-alives, but it uses no STUN: audio works only if
the server sends it back to where the phone's audio comes from.
No call transfer, conference, TLS/SRTP, installer or code signature.

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