@echo off
rem Sets up the Lode server: asks a few questions (loded setup), installs the Windows
rem service (loded service install) and starts it. Started by the installer and from the
rem Start menu ("Lode server setup"). Asks for administrator rights when it lacks them.
setlocal
set "LODED=%~dp0loded.exe"
set "DATA=%ProgramData%\Lode"

net session >nul 2>&1
if errorlevel 1 (
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)

title Lode server setup
if exist "%DATA%\loded.toml" (
    echo Lode is already set up in %DATA%.
) else (
    "%LODED%" setup --data-dir "%DATA%"
    if errorlevel 1 goto failed
)
sc query loded >nul 2>&1
if errorlevel 1 (
    "%LODED%" service install --config "%DATA%\loded.toml"
    if errorlevel 1 goto failed
)
"%LODED%" service start
if errorlevel 1 goto failed
echo.
echo The Lode server is running as the Windows service "loded".
echo Its log is %DATA%\logs\loded.log.
pause
exit /b 0

:failed
echo.
echo Setup did not finish. Fix the problem above and run "Lode server setup" from the Start menu again.
pause
exit /b 1
