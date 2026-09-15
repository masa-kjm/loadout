# Download and install a verified Loadout release archive.
# Usage: irm https://raw.githubusercontent.com/masa-kjm/loadout/main/scripts/install.ps1 | iex
#        .\install.ps1 [-Version vX.Y.Z] [-Prefix $env:USERPROFILE\.local]

[CmdletBinding()]
param(
    [string]$Version = "",
    [string]$Prefix = "$env:USERPROFILE\.local"
)

$ErrorActionPreference = "Stop"
$Repository = "masa-kjm/loadout"
$Headers = @{
    Accept = "application/vnd.github+json"
    "User-Agent" = "loadout-installer"
}

function Get-Target {
    if (-not $IsWindows -and $env:OS -ne "Windows_NT") {
        throw "This script is for Windows only. Use scripts/install.sh on Linux or macOS."
    }

    $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    if ($architecture -ne "X64") {
        throw "Unsupported architecture: $architecture. v0.2.0 publishes only x86_64-pc-windows-msvc for Windows."
    }

    return "x86_64-pc-windows-msvc"
}

function Get-LatestVersion {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repository/releases/latest" -Headers $Headers
    if ([string]::IsNullOrWhiteSpace($release.tag_name)) {
        throw "The latest GitHub release has no tag name."
    }

    return $release.tag_name
}

function Test-Checksum {
    param(
        [Parameter(Mandatory = $true)][string]$AssetPath,
        [Parameter(Mandatory = $true)][string]$ChecksumPath
    )

    $checksumText = Get-Content -Raw -Path $ChecksumPath
    $expected = ($checksumText -split '\s+')[0]
    if ($expected -notmatch '^[0-9a-fA-F]{64}$') {
        throw "The downloaded checksum file is not a SHA-256 checksum."
    }

    $actual = (Get-FileHash -Algorithm SHA256 -Path $AssetPath).Hash
    if ($actual -ne $expected.ToUpperInvariant()) {
        throw "SHA-256 verification failed."
    }
}

$Target = Get-Target
if ([string]::IsNullOrWhiteSpace($Version)) {
    Write-Host "Fetching the latest release..."
    $Version = Get-LatestVersion
}

if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
    throw "Version must be an exact vX.Y.Z release tag."
}

$ArchiveRoot = "loadout-$Version-$Target"
$Archive = "$ArchiveRoot.zip"
$ReleaseUrl = "https://github.com/$Repository/releases/download/$Version/$Archive"
$ChecksumUrl = "$ReleaseUrl.sha256"
$TemporaryDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())

try {
    New-Item -ItemType Directory -Path $TemporaryDirectory | Out-Null
    $archivePath = Join-Path $TemporaryDirectory $Archive
    $checksumPath = "$archivePath.sha256"

    Write-Host "Installing loadout $Version ($Target)..."
    Write-Host "Downloading $ReleaseUrl..."
    Invoke-WebRequest -Uri $ReleaseUrl -OutFile $archivePath -Headers $Headers
    Invoke-WebRequest -Uri $ChecksumUrl -OutFile $checksumPath -Headers $Headers
    Test-Checksum -AssetPath $archivePath -ChecksumPath $checksumPath

    $extractDirectory = Join-Path $TemporaryDirectory "extract"
    Expand-Archive -Path $archivePath -DestinationPath $extractDirectory
    $entries = @(Get-ChildItem -Force -Path $extractDirectory)
    if ($entries.Count -ne 1 -or $entries[0].Name -ne $ArchiveRoot -or -not ($entries[0].PSIsContainer)) {
        throw "The downloaded archive has an unexpected layout."
    }

    $binarySource = Join-Path (Join-Path $extractDirectory $ArchiveRoot) "loadout.exe"
    if (-not (Test-Path -Path $binarySource -PathType Leaf)) {
        throw "loadout.exe was not found in the downloaded archive."
    }

    $binaryDirectory = Join-Path $Prefix "bin"
    $binaryDestination = Join-Path $binaryDirectory "loadout.exe"
    New-Item -ItemType Directory -Force -Path $binaryDirectory | Out-Null
    Copy-Item -Path $binarySource -Destination $binaryDestination -Force

    Write-Host ""
    Write-Host "Installed loadout to $binaryDestination"

    $userPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -notlike "*$binaryDirectory*") {
        Write-Host "NOTE: $binaryDirectory is not in your user PATH."
        Write-Host "      Add it to PATH before invoking loadout from a new shell."
    }
} finally {
    if (Test-Path -Path $TemporaryDirectory) {
        Remove-Item -Path $TemporaryDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}
