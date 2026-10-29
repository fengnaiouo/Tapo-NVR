@echo off
setlocal
rem Run start.ps1 (prefer PowerShell 7, fall back to Windows PowerShell 5.1)
set "PS=powershell"
where pwsh >nul 2>nul && set "PS=pwsh"

%PS% -NoProfile -ExecutionPolicy Bypass -File "%~dp0start.ps1" %*
set "RC=%ERRORLEVEL%"

if not "%RC%"=="0" (
    echo.
    echo [ERROR] start.ps1 failed, exit code %RC%
    pause
)
exit /b %RC%
