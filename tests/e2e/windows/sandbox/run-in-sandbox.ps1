#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet("all", "smoke", "validate", "plan", "apply", "diff", "shell")][string]$Scenario
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$workRoot = "C:\loadout-work"
$repoRoot = "C:\host-loadout"
$fixtureRoot = Join-Path $repoRoot "tests\fixtures\config\valid"
$home = Join-Path $workRoot "home"
$configRoot = Join-Path $workRoot "config"
$state = Join-Path $workRoot "state"
$configPath = Join-Path $configRoot "config.yaml"
$binary = Join-Path $repoRoot "target\release\loadout.exe"

if (Test-Path $workRoot) { Remove-Item -Recurse -Force $workRoot }
New-Item -ItemType Directory -Force -Path $home, $configRoot, $state | Out-Null
Copy-Item -Path (Join-Path $fixtureRoot "*") -Destination $configRoot -Recurse -Force

$env:HOME = $home
$env:USERPROFILE = $home
$env:XDG_CONFIG_HOME = Join-Path $workRoot "xdg-config"
$env:XDG_STATE_HOME = $state

function Invoke-Loadout {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)
    & $binary @Arguments
    if ($LASTEXITCODE -ne 0) { throw "loadout exited with code $LASTEXITCODE" }
}

function Assert-Link {
    param([string]$Path, [string]$Expected)
    $item = Get-Item -Force -LiteralPath $Path
    if (-not $item.LinkType -or $item.Target -ne $Expected) {
        throw "Unexpected link at $Path"
    }
}

function Invoke-Smoke {
    Invoke-Loadout validate --config $configPath
    Invoke-Loadout plan --config $configPath
    Invoke-Loadout apply --config $configPath --yes
    Assert-Link (Join-Path $home ".gitconfig") (Join-Path $configRoot "stores\dotfiles\gitconfig")
    Invoke-Loadout apply --config $configPath --yes
    Invoke-Loadout diff
}

switch ($Scenario) {
    "shell" {
        Write-Host "Loadout manual test environment"
        Write-Host "Config: $configPath"
        Write-Host "Home:   $home"
        Write-Host "State:  $state"
        Write-Host "Try:    & '$binary' validate --config '$configPath'"
        Read-Host "Press Enter to close the Sandbox"
    }
    "all" { Invoke-Smoke }
    "smoke" { Invoke-Smoke }
    "validate" { Invoke-Loadout validate --config $configPath }
    "plan" { Invoke-Loadout plan --config $configPath }
    "apply" {
        Invoke-Loadout apply --config $configPath --yes
        Assert-Link (Join-Path $home ".gitconfig") (Join-Path $configRoot "stores\dotfiles\gitconfig")
    }
    "diff" { Invoke-Loadout diff }
}
