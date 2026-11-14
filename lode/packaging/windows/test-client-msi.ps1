# Checks the Lode client MSI on a Windows machine with administrator rights (run by the
# release workflow): it installs lode.exe on PATH, shares lode.exe and PATH with the server
# installer so that removing one keeps what the other still needs, and uninstalls cleanly.
#   pwsh packaging/windows/test-client-msi.ps1 dist\lode-0.1.0-x64.msi dist\lode-server-0.1.0-x64.msi
param(
    [Parameter(Mandatory = $true)][string]$Client,
    [Parameter(Mandatory = $true)][string]$Server
)
$ErrorActionPreference = 'Stop'
function Fail($m) { Write-Error "test-client-msi: $m"; exit 1 }
function Msi($action, $file) {
    $p = Start-Process msiexec.exe -ArgumentList "$action `"$((Resolve-Path $file).Path)`" /qn" -Wait -PassThru
    if ($p.ExitCode -ne 0) { Fail "msiexec $action $file exited with $($p.ExitCode)" }
}
$dir = Join-Path $env:ProgramFiles 'Lode'
$lode = Join-Path $dir 'lode.exe'
function OnPath { [Environment]::GetEnvironmentVariable('PATH', 'Machine') -split ';' | Where-Object { $_.TrimEnd('\') -eq $dir } }

Msi '/i' $Client
if (-not (Test-Path $lode)) { Fail 'lode.exe is missing' }
if (-not (OnPath)) { Fail "PATH lacks $dir" }
& $lode --version
if ($LASTEXITCODE -ne 0) { Fail 'lode --version failed' }
# self-update leaves an installer-managed lode to the installer.
$out = & $lode --json self-update | ConvertFrom-Json
if ($out.error.code -ne 'package_managed') { Fail "self-update did not refuse: $($out | ConvertTo-Json -Compress)" }

# With the server installed too, removing the client keeps lode.exe and PATH.
Msi '/i' $Server
Msi '/x' $Client
if (-not (Test-Path $lode)) { Fail 'removing the client took lode.exe from the server install' }
if (-not (OnPath)) { Fail 'removing the client took the PATH entry from the server install' }
Msi '/x' $Server
if (Test-Path $lode) { Fail 'lode.exe is still there after both were removed' }
if (OnPath) { Fail 'the PATH entry is still there after both were removed' }
Write-Output 'test-client-msi: ok'
