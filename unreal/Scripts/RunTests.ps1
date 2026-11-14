# Copyright SimpleDevOps contributors. Licensed under Apache-2.0.
<#
.SYNOPSIS
  Runs the plugin's automation tests in a throwaway project (unreal T02 to T10).
.DESCRIPTION
  Builds the plugin (BuildPlugin.ps1), makes a blank project under ../Build/TestHost with the built
  plugin in its Plugins folder, then runs
  "Automation RunTests Lode" in UnrealEditor-Cmd. With -E2E it first starts a real Lode server
  (Tests/E2E/e2e_server.py) and runs the Lode.E2E tests against it, -Repeat times in a row.
.EXAMPLE
  ./RunTests.ps1 -Engine "C:\Program Files\Epic Games\UE_5.4"
  ./RunTests.ps1 -Engine "C:\Program Files\Epic Games\UE_5.4" -E2E -Lode C:\lode\lode.exe -Loded C:\lode\loded.exe -Repeat 3
#>
param(
    [Parameter(Mandatory = $true)][string]$Engine,
    [string]$Filter = "Lode",
    [switch]$E2E,
    [string]$Lode = "lode",
    [string]$Loded = "loded",
    [int]$Repeat = 1
)
$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..")
$Editor = Join-Path $Engine "Engine\Binaries\Win64\UnrealEditor-Cmd.exe"
if (-not (Test-Path $Editor)) { throw "UnrealEditor-Cmd.exe not found under $Engine" }
$Python = Join-Path $Engine "Engine\Binaries\ThirdParty\Python3\Win64\python.exe"
if (-not (Test-Path $Python)) { $Python = "python" }

# A project without C++ cannot build plugins, so build the plugin first and test the built copy.
& (Join-Path $PSScriptRoot "BuildPlugin.ps1") -Engine $Engine
$Version = (Get-Content (Join-Path $Engine "Engine\Build\Build.version") | ConvertFrom-Json)
$Package = Join-Path $Root "Build\LodeSourceControl-UE$($Version.MajorVersion).$($Version.MinorVersion)"

$Host_ = Join-Path $Root "Build\TestHost"
$PluginCopy = Join-Path $Host_ "Plugins\LodeSourceControl"
if (Test-Path $PluginCopy) { Remove-Item -Recurse -Force $PluginCopy }
New-Item -ItemType Directory -Force -Path (Join-Path $Host_ "Plugins") | Out-Null
Copy-Item -Recurse $Package $PluginCopy
$Project = Join-Path $Host_ "LodeTestHost.uproject"
@'
{ "FileVersion": 3, "Category": "", "Description": "Host project for the Lode plugin tests",
  "Plugins": [ { "Name": "LodeSourceControl", "Enabled": true } ] }
'@ | Set-Content -Encoding UTF8 $Project

function Find-Tool([string]$Name) {
    $Command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($Command) { return $Command.Source }
    if (Test-Path $Name) { return (Resolve-Path $Name).Path }
    throw "$Name not found. Pass its full path."
}

$Failed = 0
for ($Run = 1; $Run -le $Repeat; $Run++) {
    $Details = Join-Path $Host_ "e2e.json"
    $TestFilter = $Filter
    if ($E2E) {
        & $Python (Join-Path $Root "LodeSourceControl\Tests\E2E\e2e_server.py") start --lode (Find-Tool $Lode) --loded (Find-Tool $Loded) --out $Details
        if ($LASTEXITCODE -ne 0) { throw "Could not start the Lode test server" }
        $env:LODE_E2E = $Details
        $TestFilter = "Lode.E2E"
    }
    try {
        $Report = Join-Path $Host_ "Report-$Run"
        & $Editor $Project "-ExecCmds=Automation RunTests $TestFilter; Quit" -unattended -nullrhi -nosplash -nosound -nopause -stdout -FullStdOutLogOutput "-ReportExportPath=$Report" -log
        if ($LASTEXITCODE -ne 0) { $Failed++ ; Write-Host "Run $Run failed (exit $LASTEXITCODE); report in $Report" }
        else { Write-Host "Run $Run passed; report in $Report" }
    }
    finally {
        if ($E2E) {
            Remove-Item Env:LODE_E2E -ErrorAction SilentlyContinue
            & $Python (Join-Path $Root "LodeSourceControl\Tests\E2E\e2e_server.py") stop $Details
        }
    }
}
if ($Failed -gt 0) { throw "$Failed of $Repeat run(s) failed" }
