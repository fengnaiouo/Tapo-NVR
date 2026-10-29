@echo off
setlocal
rem Run setup.ps1 (prefer PowerShell 7, fall back to Windows PowerShell 5.1)
set "PS=powershell"
where pwsh >nul 2>nul && set "PS=pwsh"

%PS% -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
set "RC=%ERRORLEVEL%"

echo.
if not "%RC%"=="0" echo [ERROR] setup.ps1 failed, exit code %RC%
pause
exit /b %RC%
