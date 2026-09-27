#Requires -Version 5.1
param(
    [string]$Version = 'latest',
    [string]$RootDir,
    [string]$InstallDir
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Use install.sh on Linux or macOS.' }
if ($PSBoundParameters.ContainsKey('InstallDir')) { throw 'Use -RootDir to select the complete Agentlaw installation. -InstallDir only selected binaries and is no longer supported.' }
$userProfile = [Environment]::GetFolderPath('UserProfile')
if ([string]::IsNullOrWhiteSpace($userProfile)) { throw 'Cannot find the Windows user profile. Specify a writable RootDir explicitly after repairing the profile.' }
if (-not $PSBoundParameters.ContainsKey('RootDir')) { $RootDir = Join-Path $userProfile 'Agentlaw' }
if (-not [IO.Path]::IsPathRooted($RootDir)) { throw 'RootDir must be an absolute path.' }
$RootDir = [IO.Path]::GetFullPath($RootDir)
if ($RootDir.TrimEnd('\') -eq [IO.Path]::GetPathRoot($RootDir).TrimEnd('\')) {
    throw 'RootDir cannot be a filesystem root.'
}
$ancestor = $RootDir
while ($ancestor) {
    if (Test-Path -LiteralPath $ancestor) {
        $item = Get-Item -LiteralPath $ancestor -Force -ErrorAction Stop
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "RootDir passes through a junction or symbolic link: $ancestor"
        }
        if (-not $item.PSIsContainer) { throw "RootDir passes through a file: $ancestor" }
    }
    $parent = Split-Path -Path $ancestor -Parent
    if (-not $parent -or $parent -eq $ancestor) { break }
    $ancestor = $parent
}
$appDataRoots = @(Join-Path $userProfile 'AppData')
foreach ($folder in @('LocalApplicationData', 'ApplicationData')) {
    $path = [Environment]::GetFolderPath($folder)
    if ($path) { $appDataRoots += Split-Path $path -Parent }
}
foreach ($appDataRoot in @($appDataRoots | Select-Object -Unique)) {
    $appDataRoot = [IO.Path]::GetFullPath($appDataRoot).TrimEnd('\')
    if ($RootDir.Equals($appDataRoot, [StringComparison]::OrdinalIgnoreCase) -or
        $RootDir.StartsWith(($appDataRoot + '\'), [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Choose a RootDir outside AppData so packaged harnesses do not redirect new files into private package storage.'
    }
}
$stateDir = Join-Path $RootDir 'state'
$layoutMarker = Join-Path $RootDir '.agentlaw-layout'
$layoutVersion = 'agentlaw-managed-layout-v1'
$pendingMarker = Join-Path $RootDir '.agentlaw-layout.pending'
foreach ($knownPath in @($stateDir, $layoutMarker, $pendingMarker, (Join-Path $RootDir 'bin'),
    (Join-Path $RootDir '.bin-staged'), (Join-Path $RootDir '.bin-previous'), (Join-Path $RootDir '.install.lock'))) {
    if (Test-Path -LiteralPath $knownPath) {
        $item = Get-Item -LiteralPath $knownPath -Force -ErrorAction Stop
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "Agentlaw installation path is a junction or symbolic link: $knownPath"
        }
    }
}
$managedUpgrade = Test-Path -LiteralPath $layoutMarker
if (Test-Path -LiteralPath $layoutMarker) {
    if (-not [string]::Equals((Get-Content -LiteralPath $layoutMarker -Raw).Trim(), $layoutVersion, [StringComparison]::Ordinal)) {
        throw 'The existing Agentlaw layout marker is unrecognized; no files were changed.'
    }
} else {
    foreach ($reserved in @('bin', '.bin-staged', '.bin-previous')) {
        if (Test-Path -LiteralPath (Join-Path $RootDir $reserved)) {
            throw "This directory contains unrecognized Agentlaw installation files ($reserved) without a layout marker. No files were changed."
        }
    }
    if ((Test-Path -LiteralPath $stateDir) -and
        @(Get-ChildItem -LiteralPath $stateDir -Force -ErrorAction Stop | Select-Object -First 1).Count -gt 0) {
        throw 'This directory contains unrecognized Agentlaw state without a layout marker. Preserve it and resolve the installation layout before updating.'
    }
    if (Test-Path -LiteralPath (Join-Path $RootDir 'memory')) {
        throw 'This directory already contains memory without a recognized installation marker. Connect or preserve it explicitly before choosing this installation root.'
    }
}
if ($env:AGENTLAW_HOME) {
    $configuredState = [IO.Path]::GetFullPath($env:AGENTLAW_HOME)
    if (-not $configuredState.Equals($stateDir, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'AGENTLAW_HOME selects a different installation. Preserve its memory and state; migrate or unset that override explicitly before installing here.'
    }
}
foreach ($scope in @('User', 'Machine')) {
    $configured = [Environment]::GetEnvironmentVariable('AGENTLAW_HOME', $scope)
    if ($configured -and -not [IO.Path]::GetFullPath($configured).Equals($stateDir, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$scope AGENTLAW_HOME selects another installation: $configured. Resolve that setting before installing here."
    }
}
& {
    $otherRoots = @()
    $defaultRoot = Join-Path $userProfile 'Agentlaw'
    if ((Test-Path -LiteralPath (Join-Path $defaultRoot '.agentlaw-layout')) -and
        -not $defaultRoot.Equals($RootDir, [StringComparison]::OrdinalIgnoreCase)) {
        $otherRoots += $defaultRoot
    }
    foreach ($entry in @([Environment]::GetEnvironmentVariable('Path', 'User') -split ';')) {
        if (-not $entry -or -not [IO.Path]::IsPathRooted($entry)) { continue }
        $binPath = [IO.Path]::GetFullPath($entry).TrimEnd('\')
        if ([IO.Path]::GetFileName($binPath) -ne 'bin') { continue }
        $candidateRoot = Split-Path $binPath -Parent
        if ((Test-Path -LiteralPath (Join-Path $candidateRoot '.agentlaw-layout')) -and
            -not $candidateRoot.Equals($RootDir, [StringComparison]::OrdinalIgnoreCase)) {
            $otherRoots += $candidateRoot
        }
    }
    if ($otherRoots.Count -gt 0) {
        throw "Agentlaw is already installed at $(@($otherRoots | Select-Object -Unique) -join ', '). Update that root, or migrate the existing installation before changing its location."
    }
}
if (-not $managedUpgrade) {
    $knownState = @()
    $localBases = @([Environment]::GetFolderPath('LocalApplicationData'), $env:LOCALAPPDATA) | Where-Object { $_ } | Select-Object -Unique
    foreach ($basePath in $localBases) {
        foreach ($legacyName in @('Agentlaw', 'AgentlawNext')) {
            $candidate = Join-Path $basePath $legacyName
            if (Test-Path -LiteralPath (Join-Path $candidate 'config.json') -PathType Leaf) { $knownState += $candidate }
            elseif (Test-Path -LiteralPath (Join-Path $candidate 'machine.json') -PathType Leaf) { $knownState += $candidate }
        }
        $packages = Join-Path $basePath 'Packages'
        if (Test-Path -LiteralPath $packages -PathType Container) {
            foreach ($package in @(Get-ChildItem -LiteralPath $packages -Directory -Filter 'OpenAI.Codex_*' -ErrorAction Stop)) {
                foreach ($legacyName in @('Agentlaw', 'AgentlawNext')) {
                    $candidate = Join-Path $package.FullName "LocalCache/Local/$legacyName"
                    if (Test-Path -LiteralPath (Join-Path $candidate 'config.json') -PathType Leaf) { $knownState += $candidate }
                    elseif (Test-Path -LiteralPath (Join-Path $candidate 'machine.json') -PathType Leaf) { $knownState += $candidate }
                }
            }
        }
    }
    if ($knownState.Count -gt 0) {
        throw "Existing Agentlaw state found at $(@($knownState | Select-Object -Unique) -join ', '). Migrate that installation before changing its location."
    }
}
function Assert-AgentlawBundle {
    param([string]$BinDir, [string]$ExpectedMemory, [switch]$SkipLocationCheck)
    foreach ($name in @('agentlaw.exe', 'agentlaw-worker.exe', 'LICENSE.agentlaw')) {
        $filePath = Join-Path $BinDir $name
        if (-not (Test-Path -LiteralPath $filePath -PathType Leaf)) {
            throw "Incomplete Agentlaw binary bundle: $name is missing from $BinDir."
        }
        $file = Get-Item -LiteralPath $filePath -Force -ErrorAction Stop
        if ($file.PSIsContainer -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Invalid Agentlaw binary bundle: $filePath is not an ordinary file."
        }
    }
    $previousHome = $env:AGENTLAW_HOME
    $previousLocal = $env:LOCALAPPDATA
    try {
        Remove-Item Env:AGENTLAW_HOME -ErrorAction SilentlyContinue
        $env:LOCALAPPDATA = Join-Path ([IO.Path]::GetTempPath()) ('agentlaw-isolated-probe-' + [Guid]::NewGuid().ToString('N'))
        $command = Join-Path $BinDir 'agentlaw.exe'
        if (-not $SkipLocationCheck) {
            $output = & $command store propose-location
            if ($LASTEXITCODE -ne 0 -or @($output).Count -ne 1 -or -not $output) {
                throw "Agentlaw location check failed for $BinDir."
            }
            $result = $output | ConvertFrom-Json
            $createdProperty = if ($result -is [pscustomobject]) { $result.PSObject.Properties['created'] } else { $null }
            if ($null -eq $createdProperty -or $createdProperty.Value -isnot [bool] -or $createdProperty.Value -ne $false -or
                $result.proposed_path -isnot [string] -or -not $result.proposed_path -or
                -not [IO.Path]::GetFullPath($result.proposed_path).Equals([IO.Path]::GetFullPath($ExpectedMemory), [StringComparison]::OrdinalIgnoreCase)) {
                throw "Agentlaw selected an unexpected memory location from $BinDir."
            }
        }
        $schema = & $command schema
        if ($LASTEXITCODE -ne 0 -or -not $schema) { throw "Agentlaw schema check failed for $BinDir." }
    } finally {
        if ($null -eq $previousHome) { Remove-Item Env:AGENTLAW_HOME -ErrorAction SilentlyContinue }
        else { $env:AGENTLAW_HOME = $previousHome }
        if ($null -eq $previousLocal) { Remove-Item Env:LOCALAPPDATA -ErrorAction SilentlyContinue }
        else { $env:LOCALAPPDATA = $previousLocal }
    }
}
function Remove-ReservedBundle {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $directory = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (-not $directory.PSIsContainer -or ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Reserved Agentlaw path is not an ordinary directory: $Path"
    }
    $allowed = @('agentlaw.exe', 'agentlaw-worker.exe', 'LICENSE.agentlaw')
    $entries = @(Get-ChildItem -LiteralPath $Path -Force -ErrorAction Stop)
    foreach ($entry in $entries) {
        if ($entry.PSIsContainer -or ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            $allowed -cnotcontains $entry.Name) {
            throw "Reserved Agentlaw bundle contains an unexpected entry; preserved: $($entry.FullName)"
        }
    }
    foreach ($entry in $entries) { Remove-Item -LiteralPath $entry.FullName -Force -ErrorAction Stop }
    Remove-Item -LiteralPath $Path -Force -ErrorAction Stop
}
function Resolve-InterruptedBinSwap {
    param([string]$Root, [string]$Bin, [string]$Previous, [string]$Staged)
    if (-not (Test-Path -LiteralPath $Previous)) { return }
    $memory = Join-Path $Root 'memory'
    if (-not (Test-Path -LiteralPath $Bin)) {
        Assert-AgentlawBundle $Previous $memory -SkipLocationCheck
        [IO.Directory]::Move($Previous, $Bin)
        Assert-AgentlawBundle $Bin $memory
        return
    }
    try { Assert-AgentlawBundle $Bin $memory; return }
    catch { $activeError = $_ }
    try { Assert-AgentlawBundle $Previous $memory -SkipLocationCheck }
    catch { throw "Neither the active nor previous Agentlaw bundle could be verified. Both were preserved: $Bin ; $Previous. Active error: $activeError. Previous error: $_" }
    if (Test-Path -LiteralPath $Staged) { Remove-ReservedBundle $Staged }
    [IO.Directory]::Move($Bin, $Staged)
    try {
        [IO.Directory]::Move($Previous, $Bin)
        Assert-AgentlawBundle $Bin $memory
    } catch {
        $recoveryError = $_
        try {
            if ((Test-Path -LiteralPath $Bin) -and -not (Test-Path -LiteralPath $Previous)) {
                [IO.Directory]::Move($Bin, $Previous)
            }
            if (-not (Test-Path -LiteralPath $Bin)) { [IO.Directory]::Move($Staged, $Bin) }
        } catch {
            throw "Agentlaw recovery failed: $recoveryError. Restoring the active bundle also failed: $_. Preserve $Bin ; $Previous ; $Staged for repair."
        }
        throw "Agentlaw recovery failed: $recoveryError. The original active bundle was restored but could not be verified; previous bundle was preserved."
    }
}
$binaryDir = Join-Path $RootDir 'bin'
$stagedBin = Join-Path $RootDir '.bin-staged'
$previousBin = Join-Path $RootDir '.bin-previous'
if ((Test-Path -LiteralPath $previousBin) -or
    (Test-Path -LiteralPath (Join-Path $RootDir '.agentlaw-layout.pending'))) {
    $recoveryLock = [IO.File]::Open((Join-Path $RootDir '.install.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        if (-not $managedUpgrade -and (Test-Path -LiteralPath $pendingMarker)) {
            $pending = Get-Item -LiteralPath $pendingMarker -Force -ErrorAction Stop
            if ($pending.PSIsContainer -or ($pending.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                -not [string]::Equals((Get-Content -LiteralPath $pendingMarker -Raw).Trim(), $layoutVersion, [StringComparison]::Ordinal) -or
                (Test-Path -LiteralPath $binaryDir) -or (Test-Path -LiteralPath $stagedBin) -or
                (Test-Path -LiteralPath $previousBin) -or (Test-Path -LiteralPath (Join-Path $RootDir 'memory'))) {
                throw "Interrupted Agentlaw layout marker needs inspection: $pendingMarker"
            }
            [IO.File]::Move($pendingMarker, $layoutMarker)
            $managedUpgrade = $true
        }
        if (-not [string]::Equals((Get-Content -LiteralPath $layoutMarker -Raw).Trim(), $layoutVersion, [StringComparison]::Ordinal)) { throw 'The Agentlaw layout marker changed during recovery.' }
        Resolve-InterruptedBinSwap $RootDir $binaryDir $previousBin $stagedBin
    } finally { $recoveryLock.Dispose() }
}
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
    # A released binary must understand this layout before it is published.
    $probeRoot = Join-Path $temporary 'probe'
    $probeBin = Join-Path $probeRoot 'bin'
    $null = New-Item -ItemType Directory -Path $probeBin -Force
    [IO.File]::WriteAllText((Join-Path $probeRoot '.agentlaw-layout'), $layoutVersion + "`n", [Text.Encoding]::ASCII)
    foreach ($name in @('agentlaw.exe', 'agentlaw-worker.exe')) {
        Copy-Item -LiteralPath (Join-Path $temporary ('unpacked/' + $name)) -Destination $probeBin
    }
    Copy-Item -LiteralPath (Join-Path $temporary 'unpacked/LICENSE') -Destination (Join-Path $probeBin 'LICENSE.agentlaw')
    Assert-AgentlawBundle $probeBin (Join-Path $probeRoot 'memory')
    $rootPrefix = $RootDir.TrimEnd('\') + '\'
    foreach ($child in @($binaryDir, $stagedBin, $previousBin)) {
        if (-not [IO.Path]::GetFullPath($child).StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Unsafe installation target.'
        }
    }
    $null = New-Item -ItemType Directory -Path $RootDir -Force
    $installLock = [IO.File]::Open((Join-Path $RootDir '.install.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        if ($managedUpgrade) {
            if (-not [string]::Equals((Get-Content -LiteralPath $layoutMarker -Raw).Trim(), $layoutVersion, [StringComparison]::Ordinal)) { throw 'The Agentlaw layout marker changed during download.' }
        } else {
            if (Test-Path -LiteralPath $layoutMarker) { throw 'Another Agentlaw installation appeared during download. Retry after inspecting it.' }
            foreach ($reserved in @($binaryDir, $stagedBin, $previousBin)) {
                if (Test-Path -LiteralPath $reserved) { throw 'Another installation changed the target while downloading. No files were replaced.' }
            }
        }
        if (-not (Test-Path -LiteralPath $layoutMarker)) {
            if (Test-Path -LiteralPath $pendingMarker) { throw 'An incomplete layout marker exists. Preserve it and inspect the interrupted installation.' }
            [IO.File]::WriteAllText($pendingMarker, $layoutVersion + "`n", [Text.Encoding]::ASCII)
            [IO.File]::Move($pendingMarker, $layoutMarker)
        }
        $null = New-Item -ItemType Directory -Path $stateDir -Force
        Resolve-InterruptedBinSwap $RootDir $binaryDir $previousBin $stagedBin
        if (Test-Path -LiteralPath $previousBin) {
            Assert-AgentlawBundle $binaryDir (Join-Path $RootDir 'memory')
            Remove-ReservedBundle $previousBin
        }
        if (Test-Path -LiteralPath $stagedBin) { Remove-ReservedBundle $stagedBin }
        $null = New-Item -ItemType Directory -Path $stagedBin
        foreach ($name in @('agentlaw.exe', 'agentlaw-worker.exe')) {
            Copy-Item -LiteralPath (Join-Path $temporary ('unpacked/' + $name)) -Destination $stagedBin
        }
        Copy-Item -LiteralPath (Join-Path $temporary 'unpacked/LICENSE') -Destination (Join-Path $stagedBin 'LICENSE.agentlaw')
        $hadPrevious = Test-Path -LiteralPath $binaryDir
        if ($hadPrevious) { [IO.Directory]::Move($binaryDir, $previousBin) }
        try {
            [IO.Directory]::Move($stagedBin, $binaryDir)
            Assert-AgentlawBundle $binaryDir (Join-Path $RootDir 'memory')
        } catch {
            $publishError = $_
            try {
                if (Test-Path -LiteralPath $binaryDir) {
                    [IO.Directory]::Move($binaryDir, $stagedBin)
                }
                if ($hadPrevious -and (Test-Path -LiteralPath $previousBin)) {
                    [IO.Directory]::Move($previousBin, $binaryDir)
                }
            } catch {
                throw "Agentlaw bundle publication failed: $publishError. Rollback also failed: $_. Preserved paths for repair: $binaryDir ; $previousBin ; $stagedBin"
            }
            throw $publishError
        }
        if ($hadPrevious) {
            try { Remove-ReservedBundle $previousBin }
            catch { Write-Warning "New Agentlaw bundle is active, but previous bundle cleanup was deferred: $_" }
        }
    } finally {
        $installLock.Dispose()
    }
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (@($userPath -split ';') -notcontains $binaryDir) {
        [Environment]::SetEnvironmentVariable('Path', ($binaryDir + ';' + $userPath), 'User')
    }
    if (@($env:PATH -split ';') -notcontains $binaryDir) { $env:PATH = $binaryDir + ';' + $env:PATH }
    Write-Host "Installed Agentlaw to $RootDir"
    Write-Host "State: $stateDir"
    Write-Host "Proposed memory store: $(Join-Path $RootDir 'memory') (not created; confirm it with agentlaw store create)"
    Write-Host 'Open a new terminal for the updated PATH. Run: agentlaw --version'
    Write-Host 'Existing harness registration is unchanged. Preview a Codex update with: agentlaw install --harness codex'
    Write-Host 'Model setup and harness configuration: https://github.com/paranmir/agentlaw/blob/main/docs/usage.md'
} finally {
    $resolved = [IO.Path]::GetFullPath($temporary)
    $prefix = $tempBase.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolved) -notmatch '^agentlaw-install-[0-9a-f]{32}$') { throw 'Unsafe cleanup path.' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
