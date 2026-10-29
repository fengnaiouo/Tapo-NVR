@echo off
setlocal
rem Build signed installer + latest.json. Usage: release.bat <BaseUrl> [Notes]
if "%~1"=="" (
    echo Usage: release.bat https://example.com/downloads/v0.2.0 "release notes"
    exit /b 2
)
set "PS=powershell"
where pwsh >nul 2>nul && set "PS=pwsh"

%PS% -NoProfile -ExecutionPolicy Bypass -File "%~dp0release.ps1" -BaseUrl "%~1" -Notes "%~2"
set "RC=%ERRORLEVEL%"
echo.
if not "%RC%"=="0" echo [ERROR] release.ps1 failed, exit code %RC%
pause
exit /b %RC%
