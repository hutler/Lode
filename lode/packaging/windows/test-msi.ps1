# Checks the Lode Server MSI on a Windows machine with administrator rights (run by the
# release workflow): installs it silently, sets the server up the way lode-server-setup.cmd
# does, starts the service, signs in, uninstalls, and checks that the service is gone and
# the data directory stays.
#   powershell -File packaging/windows/test-msi.ps1 dist\lode-server-0.1.0-x64.msi
param([Parameter(Mandatory = $true)][string]$Msi)
$ErrorActionPreference = 'Stop'
function Fail($m) { Write-Error "test-msi: $m"; exit 1 }
function Run($exe, [string[]]$a) {
    & $exe @a
    if ($LASTEXITCODE -ne 0) { Fail "$exe $a exited with $LASTEXITCODE" }
}

$msi = (Resolve-Path $Msi).Path
$p = Start-Process msiexec.exe -ArgumentList "/i `"$msi`" /qn /l*v install.log" -Wait -PassThru
if ($p.ExitCode -ne 0) { Get-Content install.log -Tail 50; Fail "install exited with $($p.ExitCode)" }

$dir = Join-Path $env:ProgramFiles 'Lode'
$loded = Join-Path $dir 'loded.exe'
$lode = Join-Path $dir 'lode.exe'
foreach ($f in $loded, $lode, (Join-Path $dir 'lode-server-setup.cmd')) {
    if (-not (Test-Path $f)) { Fail "$f is missing" }
}
$path = [Environment]::GetEnvironmentVariable('PATH', 'Machine')
if (-not ($path -split ';' | Where-Object { $_.TrimEnd('\') -eq $dir })) { Fail "PATH lacks $dir" }

$data = Join-Path $env:ProgramData 'Lode'
Set-Content -Path pw.txt -Value 'test-password-123'
Run $loded @('setup', '--data-dir', $data, '--local', '--yes', '--admin-password-file', 'pw.txt')
Run $loded @('service', 'install', '--config', "$data\loded.toml")
Run $loded @('service', 'start')
$ok = $false
for ($i = 0; $i -lt 50 -and -not $ok; $i++) {
    Get-Content pw.txt | & $lode login http://127.0.0.1:7480 --name admin --password-stdin *> $null
    if ($LASTEXITCODE -eq 0) { $ok = $true } else { Start-Sleep -Milliseconds 300 }
}
if (-not $ok) { Get-Content "$data\logs\loded.log" -ErrorAction SilentlyContinue; Fail 'cannot sign in' }

# Upgrade path: reinstalling the same package must leave the service running.
$p = Start-Process msiexec.exe -ArgumentList "/fa `"$msi`" /qn" -Wait -PassThru
if ($p.ExitCode -ne 0) { Fail "repair exited with $($p.ExitCode)" }
if ((Get-Service loded).Status -ne 'Running') { Fail 'the service is not running after a repair' }

$p = Start-Process msiexec.exe -ArgumentList "/x `"$msi`" /qn /l*v uninstall.log" -Wait -PassThru
if ($p.ExitCode -ne 0) { Get-Content uninstall.log -Tail 50; Fail "uninstall exited with $($p.ExitCode)" }
if (Get-Service loded -ErrorAction SilentlyContinue) { Fail 'the service is still installed' }
if (Test-Path $loded) { Fail 'loded.exe is still installed' }
if (-not (Test-Path "$data\loded.toml")) { Fail 'the data directory was removed' }
Write-Output 'test-msi: ok'
