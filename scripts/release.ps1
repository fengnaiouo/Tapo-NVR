# 產生已簽章的 Windows 安裝檔與自動更新用的 latest.json
#
# 用法：
#   .\scripts\release.ps1 -BaseUrl "https://github.com/你的帳號/tapo-nvr/releases/download/v0.2.0" -Notes "新增錄影回放"
#
# 產出（dist 資料夾）：
#   Tapo-NVR_x.y.z_x64-setup.exe      安裝檔（給新使用者，也是更新包）
#   latest.json                       上傳到「更新來源網址」，已安裝的電腦會自動偵測新版
#
# 發布步驟：先改 desktop\src-tauri\tauri.conf.json 與 Cargo.toml 的 version，
# 執行本腳本後，把 dist 內的兩個檔案上傳到 BaseUrl 指向的位置。
param(
    [Parameter(Mandatory = $true)][string]$BaseUrl,
    [string]$Notes = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

foreach ($f in 'bin\go2rtc.exe', 'bin\ffmpeg.exe') {
    if (-not (Test-Path $f)) { throw "缺少 $f，請先執行 scripts\setup.bat" }
}

$keyFile = Join-Path $env:USERPROFILE '.tauri\tapo-nvr.key'
$pwFile = Join-Path $env:USERPROFILE '.tauri\tapo-nvr.key.password'
if (-not (Test-Path $keyFile)) { throw "找不到簽章私鑰 $keyFile（遺失私鑰將無法再推送更新）" }
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content $keyFile -Raw
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = if (Test-Path $pwFile) { (Get-Content $pwFile -Raw).Trim() } else { '' }

$conf = Get-Content 'desktop\src-tauri\tauri.conf.json' -Raw | ConvertFrom-Json
$version = $conf.version
Write-Host "建置 Tapo NVR $version …" -ForegroundColor Cyan

Push-Location 'desktop\src-tauri'
cargo tauri build
$code = $LASTEXITCODE
Pop-Location
if ($code -ne 0) { throw "建置失敗（$code）" }

$bundle = Join-Path $root 'target\release\bundle\nsis'
$setup = Get-ChildItem $bundle -Filter "*_${version}_*-setup.exe" | Select-Object -First 1
if (-not $setup) { throw "找不到安裝檔（$bundle）" }
$sig = Get-Content "$($setup.FullName).sig" -Raw

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
# GitHub 會把上傳檔名中的空白改成「.」，所以改用不含空白的檔名
$assetName = $setup.Name -replace ' ', '-'
Copy-Item $setup.FullName (Join-Path $dist $assetName) -Force

$latest = [ordered]@{
    version   = $version
    notes     = $Notes
    pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    platforms = [ordered]@{
        'windows-x86_64' = [ordered]@{
            signature = $sig.Trim()
            url       = "$($BaseUrl.TrimEnd('/'))/$([uri]::EscapeDataString($assetName))"
        }
    }
}
$json = $latest | ConvertTo-Json -Depth 5
[IO.File]::WriteAllText((Join-Path $dist 'latest.json'), $json, (New-Object Text.UTF8Encoding $false))

Write-Host ''
Write-Host "完成：" -ForegroundColor Green
Get-ChildItem $dist | ForEach-Object { Write-Host ("  {0}  ({1:N1} MB)" -f $_.Name, ($_.Length / 1MB)) }
Write-Host ''
Write-Host "請把以上檔案上傳到：$BaseUrl"
Write-Host '應用程式的「更新來源網址」請設定為 latest.json 的網址。'
