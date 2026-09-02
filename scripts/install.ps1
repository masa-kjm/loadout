# install.ps1 — Download and install loadout from GitHub Releases.
#
# Usage:
#   irm https://raw.githubusercontent.com/massa-kj/loadout/main/install.ps1 | iex
#   .\install.ps1 [-Version v0.1.0] [-Prefix $env:USERPROFILE\.local]
#
# Layout after install:
#   <Prefix>\bin\loadout.exe   (binary)

[CmdletBinding()]
param(
    [string]$Version = "",
    [string]$Prefix  = "$env:USERPROFILE\.local"
)

$ErrorActionPreference = "Stop"

$Repo = "massa-kj/loadout"

# ── Platform detection ────────────────────────────────────────────────────────

function Get-Target {
    if (-not $IsWindows -and $env:OS -ne "Windows_NT") {
        Write-Error "error: this script is for Windows only. Use install.sh on Linux/macOS."
        exit 1
    }

    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    switch ($arch) {
        "X64"   { return "windows-x86_64" }
        "Arm64" { return "windows-aarch64" }
        default {
            Write-Error "error: unsupported architecture: $arch"
            exit 1
        }
    }
}

$Target = Get-Target

# ── Version resolution ────────────────────────────────────────────────────────

if ($Version -eq "") {
    Write-Host "Fetching latest release..."
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing
        $Version = $release.tag_name
    } catch {
        Write-Error "error: failed to fetch latest version: $_"
        exit 1
    }
    if ($Version -eq "") {
        Write-Error "error: failed to fetch latest version"
        exit 1
    }
}

Write-Host "Installing loadout $Version ($Target)..."

# ── Download ──────────────────────────────────────────────────────────────────

$ZipName = "loadout-$Version-$Target.zip"
$Url     = "https://github.com/$Repo/releases/download/$Version/$ZipName"
$TmpDir  = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $TmpDir | Out-Null

try {
    $ZipPath = Join-Path $TmpDir $ZipName
    Write-Host "Downloading $Url..."
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing

    # ── Install ───────────────────────────────────────────────────────────────

    $BinDir     = Join-Path $Prefix "bin"
    $ExtractDir = Join-Path $TmpDir "extract"

    # Create bin directory if needed
    New-Item -ItemType Directory -Path $BinDir     -Force | Out-Null
    New-Item -ItemType Directory -Path $ExtractDir -Force | Out-Null

    # Extract zip
    Expand-Archive -Path $ZipPath -DestinationPath $ExtractDir -Force

    # Locate the binary (may be at the top level or inside a single subdirectory)
    $BinarySrc = Get-ChildItem -Path $ExtractDir -Filter "loadout.exe" -Recurse | Select-Object -First 1
    if ($null -eq $BinarySrc) {
        Write-Error "error: loadout.exe not found in the downloaded archive"
        exit 1
    }

    $BinaryDest = Join-Path $BinDir "loadout.exe"
    Copy-Item -Path $BinarySrc.FullName -Destination $BinaryDest -Force

    # ── Done ──────────────────────────────────────────────────────────────────

    Write-Host ""
    Write-Host "Installed loadout to $BinaryDest"
    Write-Host ""

    # Check PATH
    $UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
    if ($UserPath -notlike "*$BinDir*") {
        Write-Host "NOTE: $BinDir is not in your PATH."
        Write-Host "      Adding it to your user PATH..."
        [System.Environment]::SetEnvironmentVariable(
            "PATH",
            "$BinDir;$UserPath",
            "User"
        )
        Write-Host "      Done. Restart your shell for the change to take effect."
    }
} finally {
    # Cleanup temporary directory
    Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
