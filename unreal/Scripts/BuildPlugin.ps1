# Copyright SimpleDevOps contributors. Licensed under Apache-2.0.
<#
.SYNOPSIS
  Builds the Lode plugin for one installed engine and zips it (unreal T01, T11).
.EXAMPLE
  ./BuildPlugin.ps1 -Engine "C:\Program Files\Epic Games\UE_5.8"
  Writes ../Build/LodeSourceControl-UE5.8-Win64.zip
#>
param(
    [Parameter(Mandatory = $true)][string]$Engine,
    [string]$Output = "",
    [string]$Platforms = "Win64"
)
$ErrorActionPreference = "Stop"
# Set here, not in param(): Windows PowerShell 5.1 leaves $PSScriptRoot empty in parameter defaults.
if (-not $Output) { $Output = Join-Path $PSScriptRoot "..\Build" }
$Plugin = Resolve-Path (Join-Path $PSScriptRoot "..\LodeSourceControl\LodeSourceControl.uplugin")
$Uat = Join-Path $Engine "Engine\Build\BatchFiles\RunUAT.bat"
if (-not (Test-Path $Uat)) { throw "RunUAT.bat not found under $Engine. Pass the engine folder, such as C:\Program Files\Epic Games\UE_5.8." }

$Version = (Get-Content (Join-Path $Engine "Engine\Build\Build.version") | ConvertFrom-Json)
$EngineName = "UE$($Version.MajorVersion).$($Version.MinorVersion)"
$Package = Join-Path $Output "LodeSourceControl-$EngineName"
New-Item -ItemType Directory -Force -Path $Output | Out-Null
if (Test-Path $Package) { Remove-Item -Recurse -Force $Package }

& $Uat BuildPlugin "-Plugin=$Plugin" "-Package=$Package" "-TargetPlatforms=$Platforms" -Rocket
if ($LASTEXITCODE -ne 0) { throw "BuildPlugin failed with exit code $LASTEXITCODE" }

# Ship sources and binaries, not intermediate files.
Remove-Item -Recurse -Force (Join-Path $Package "Intermediate") -ErrorAction SilentlyContinue
$Zip = "$Package-$($Platforms -replace '\+','-').zip"
if (Test-Path $Zip) { Remove-Item -Force $Zip }
Compress-Archive -Path $Package -DestinationPath $Zip
Write-Host "Built $Zip"
