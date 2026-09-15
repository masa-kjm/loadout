#Requires -Version 5.1

[CmdletBinding()]
param(
    [ValidateSet("all", "smoke", "validate", "plan", "apply", "diff", "shell")][string]$Scenario = "all",
    [switch]$Build
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$scriptRoot = $PSScriptRoot
$repoRoot = (Resolve-Path (Join-Path $scriptRoot "..\..\..\..")).Path
$binary = Join-Path $repoRoot "target\release\loadout.exe"
$logRoot = Join-Path (Resolve-Path (Join-Path $scriptRoot "..\..")) "logs"

if ($Build -or -not (Test-Path $binary)) {
    Push-Location $repoRoot
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path $binary)) { throw "Release binary not found: $binary" }

if ($Scenario -eq "shell") {
    & (Join-Path $scriptRoot "create-wsb.ps1") -Scenario "shell" -RepositoryRoot $repoRoot -LogRoot $logRoot
    Start-Process -FilePath "WindowsSandbox.exe" -ArgumentList (Join-Path $scriptRoot "loadout.wsb") -Wait
    exit 0
}

& (Join-Path $scriptRoot "create-wsb.ps1") -Scenario $Scenario -RepositoryRoot $repoRoot -LogRoot $logRoot
Start-Process -FilePath "WindowsSandbox.exe" -ArgumentList (Join-Path $scriptRoot "loadout.wsb") -Wait
