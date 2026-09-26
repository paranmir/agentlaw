#Requires -Version 5.1
param(
    [string]$Version = 'latest',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs/Agentlaw/bin')
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Use install.sh on Linux or macOS.' }
$architecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($architecture -ne 'AMD64') { throw 'This installer currently supports Windows x64. Build from source for other targets.' }
if ($Version -notmatch '^(latest|v\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?)$') { throw 'Use latest or a v-prefixed version.' }
$base = 'https://github.com/paranmir/agentlaw/releases'
$base = if ($Version -eq 'latest') { "$base/latest/download" } else { "$base/download/$Version" }
$archive = 'agentlaw-x86_64-pc-windows-msvc.zip'
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$temporary = Join-Path $tempBase ('agentlaw-install-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $temporary
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest "$base/$archive" -OutFile (Join-Path $temporary $archive) -UseBasicParsing
    Invoke-WebRequest "$base/SHA256SUMS" -OutFile (Join-Path $temporary 'SHA256SUMS') -UseBasicParsing
    $checksums = Get-Content -LiteralPath (Join-Path $temporary 'SHA256SUMS') -Raw
    $match = [regex]::Match($checksums, '(?m)^([a-fA-F0-9]{64})\s+\*?' + [regex]::Escape($archive) + '\s*$')
    $actual = (Get-FileHash -LiteralPath (Join-Path $temporary $archive) -Algorithm SHA256).Hash
    if (-not $match.Success -or $actual -ne $match.Groups[1].Value) { throw 'Checksum verification failed; nothing installed.' }
    Expand-Archive -LiteralPath (Join-Path $temporary $archive) -DestinationPath (Join-Path $temporary 'unpacked')
    $InstallDir = [IO.Path]::GetFullPath($InstallDir)
    $null = New-Item -ItemType Directory -Path $InstallDir -Force
    foreach ($name in @('agentlaw.exe', 'agentlaw-worker.exe')) {
        Copy-Item -LiteralPath (Join-Path $temporary ('unpacked/' + $name)) -Destination $InstallDir -Force
    }
    Copy-Item -LiteralPath (Join-Path $temporary 'unpacked/LICENSE') -Destination (Join-Path $InstallDir 'LICENSE.agentlaw') -Force
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (@($userPath -split ';') -notcontains $InstallDir) {
        [Environment]::SetEnvironmentVariable('Path', ($InstallDir + ';' + $userPath), 'User')
    }
    if (@($env:PATH -split ';') -notcontains $InstallDir) { $env:PATH = $InstallDir + ';' + $env:PATH }
    Write-Host "Installed Agentlaw to $InstallDir"
    Write-Host 'Open a new terminal for the updated PATH. Run: agentlaw --version'
    Write-Host 'Model setup and harness configuration: https://github.com/paranmir/agentlaw/blob/main/docs/usage.md'
} finally {
    $resolved = [IO.Path]::GetFullPath($temporary)
    $prefix = $tempBase.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolved) -notmatch '^agentlaw-install-[0-9a-f]{32}$') { throw 'Unsafe cleanup path.' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
