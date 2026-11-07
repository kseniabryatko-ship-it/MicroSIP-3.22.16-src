param(
    [string]$OutputDirectory = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = $PSScriptRoot
$previousLocation = Get-Location
$previousReleaseDebug = $env:CARGO_PROFILE_RELEASE_DEBUG

function Invoke-Cargo {
    param([string[]]$CargoArguments)

    Write-Host ('> cargo ' + ($CargoArguments -join ' '))
    & cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw ('Cargo failed: ' + ($CargoArguments -join ' '))
    }
}

try {
    Set-Location -LiteralPath $root

    if (-not (Test-Path -LiteralPath 'Cargo.lock')) {
        throw 'Cargo.lock is required for a reproducible application build.'
    }

    $hostLine = (& rustc -vV | Select-String '^host:').ToString()
    if ($LASTEXITCODE -ne 0) {
        throw 'Unable to determine the Rust host target.'
    }
    if ($hostLine.Trim() -ne 'host: x86_64-pc-windows-msvc') {
        throw "This packaging script requires Windows x64 MSVC. Found: $hostLine"
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

    $metadataLines = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo metadata failed.'
    }
    $metadata = ($metadataLines -join "`n") | ConvertFrom-Json
    $targetDirectory = [string]$metadata.target_directory

    if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
        $OutputDirectory = Join-Path $targetDirectory 'portable-releases'
    }
    $outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
    New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null

    $name = 'microsip-rs-preview-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss')
    $package = Join-Path $outputRoot $name
    if (Test-Path -LiteralPath $package) {
        throw "Output folder already exists: $package"
    }
    New-Item -ItemType Directory -Path $package | Out-Null

    foreach ($binary in @('microsip-rs.exe', 'phone-console.exe')) {
        $source = Join-Path $targetDirectory ('release\' + $binary)
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            throw "Missing executable: $source"
        }
        Copy-Item -LiteralPath $source -Destination $package
    }

    $readme = @'
MicroSIP Rust - development preview
==================================

This is NOT a complete replacement for MicroSIP.
Direct UDP calls only. No GUI, provider registration or NAT traversal
is guaranteed by this package. Do not expose the SIP listener directly
to an untrusted network.

Windows x64 is required. Microsoft Visual C++ Runtime x64 may be required.
Use headphones to prevent acoustic feedback.

Manual incoming call:
  phone-console.exe 127.0.0.1:5070

Caller, in another terminal:
  microsip-rs.exe call 127.0.0.1:5072 127.0.0.1:5070 30

Commands in phone-console:
  answer
  reject
  hangup
  status
  quit

Automatic-answer receiver, instead of phone-console:
  microsip-rs.exe listen 127.0.0.1:5070

For LAN testing, use each computer's actual local IP address.
Allow the application only on a trusted private network if Windows asks.
Do not disable the firewall globally.

Known limitations:
  One direct call at a time; incomplete SIP interoperability.
  No claim of complete CANCEL/non-2xx ACK handling.
  No installer, code signing, TLS or SRTP in this preview package.
  Positive RTP counters do not prove perceived audio quality.

This archive contains development binaries and instructions only.
It is not a public redistribution release. Complete the source-license
and dependency-notice audit before distributing it publicly.
'@
    Set-Content -LiteralPath (Join-Path $package 'README.txt') `
        -Value $readme -Encoding UTF8

    $commit = 'unknown'
    $gitCommand = Get-Command git -ErrorAction SilentlyContinue
    if ($null -ne $gitCommand) {
        $gitOutput = & git rev-parse HEAD
        if ($LASTEXITCODE -eq 0) {
            $commit = ($gitOutput -join '').Trim()
        }
    }

    $buildInfo = @(
        'Built UTC: ' + [DateTime]::UtcNow.ToString('o')
        'Commit: ' + $commit
        'Rust: ' + (& rustc --version)
        'Cargo: ' + (& cargo --version)
        'Target: x86_64-pc-windows-msvc'
        'Profile: release; debug symbols disabled'
    )
    Set-Content -LiteralPath (Join-Path $package 'BUILD-INFO.txt') `
        -Value $buildInfo -Encoding UTF8

    $archive = Join-Path $outputRoot ($name + '.zip')
    Compress-Archive -Path $package -DestinationPath $archive
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    Set-Content -LiteralPath ($archive + '.sha256') `
        -Value ($hash + '  ' + [System.IO.Path]::GetFileName($archive)) `
        -Encoding ASCII

    Write-Host ''
    Write-Host 'BUILD AND PACKAGING PASSED'
    Write-Host "Folder: $package"
    Write-Host "ZIP: $archive"
    Write-Host "SHA256: $hash"
    Write-Host 'Runtime call testing is still required.'
}
finally {
    $env:CARGO_PROFILE_RELEASE_DEBUG = $previousReleaseDebug
    Set-Location $previousLocation
}