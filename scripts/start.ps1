$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
if (-not (Test-Path '.\target\release\tapo-nvr.exe')) {
    Write-Host '找不到 tapo-nvr.exe，請先執行 scripts\setup.bat' -ForegroundColor Red
    exit 1
}

# 讓伺服器知道有外層腳本負責重啟，網頁上的「重新啟動」按鈕才會啟用
$env:TAPO_NVR_SUPERVISED = '1'
while ($true) {
    & .\target\release\tapo-nvr.exe --data data --import config.toml
    $code = $LASTEXITCODE
    if ($code -ne 3) { exit $code }
    Write-Host '重新啟動中…' -ForegroundColor Cyan
    Start-Sleep -Seconds 1
}
