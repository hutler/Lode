# Run on a clean Windows release runner. Exercise both link ownership cases.
param([Parameter(Mandatory)][string]$Installer)
$ErrorActionPreference = 'Stop'
$scheme = 'HKCU:\Software\Classes\lode'
$key = "$scheme\shell\open\command"
if (Test-Path $scheme) { throw 'NSIS test needs a clean user profile' }

foreach ($transfer in @($false, $true)) {
    $p = Start-Process $Installer -ArgumentList '/S' -Wait -PassThru -WindowStyle Hidden
    if ($p.ExitCode) { throw "NSIS install exited with $($p.ExitCode)" }
    $command = (Get-Item -LiteralPath $key).GetValue('')
    if ($command -notmatch '^"([^"]+\\lode-desk\.exe)" "%1"$') {
        throw "Unexpected installed URI command: $command"
    }
    $exe = $Matches[1]
    if (-not (Test-Path -LiteralPath $exe)) { throw 'Installed Desk executable missing' }
    $uninstaller = Join-Path (Split-Path $exe) 'uninstall.exe'
    if ($transfer) {
        $replacement = '"C:\example\other-app.exe" "%1"'
        Set-Item -LiteralPath $key -Value $replacement
    }
    $p = Start-Process $uninstaller -ArgumentList '/S' -Wait -PassThru -WindowStyle Hidden
    if ($p.ExitCode -or (Test-Path -LiteralPath $exe)) { throw 'NSIS uninstall failed' }
    if ($transfer) {
        if ((Get-Item -LiteralPath $key).GetValue('') -ne $replacement) {
            throw 'NSIS uninstall changed a replacement URI handler'
        }
    } elseif (Test-Path -LiteralPath $scheme) {
        throw 'NSIS uninstall left its URI handler registered'
    }
}
