#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Scenario,
    [string]$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..\..")),
    [string]$LogRoot = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..\..")) "logs")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

New-Item -ItemType Directory -Force -Path $LogRoot | Out-Null
$templatePath = Join-Path $PSScriptRoot "loadout.wsb.template"
$outputPath = Join-Path $PSScriptRoot "loadout.wsb"
$content = Get-Content -Raw -Path $templatePath
$content = $content.Replace("__REPO_ROOT__", $RepositoryRoot)
$content = $content.Replace("__LOG_ROOT__", $LogRoot)
$content = $content.Replace("__SCENARIO__", $Scenario)
Set-Content -Path $outputPath -Value $content -Encoding UTF8 -NoNewline
Write-Host "Generated $outputPath"
