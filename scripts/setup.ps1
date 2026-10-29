# 首次安裝（開發 / 獨立伺服器用）：下載 go2rtc 與 ffmpeg、編譯伺服器
# 一般使用者請直接安裝 Tapo NVR 安裝檔（scripts\release.bat 產生），不需要執行本腳本。
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
New-Item -ItemType Directory -Force bin | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem

if (-not (Test-Path 'bin\go2rtc.exe')) {
    Write-Host '下載 go2rtc…'
    $zip = Join-Path $env:TEMP 'go2rtc_win64.zip'
    Invoke-WebRequest 'https://github.com/AlexxIT/go2rtc/releases/latest/download/go2rtc_win64.zip' -OutFile $zip
    Expand-Archive -Force $zip -DestinationPath bin
}
& .\bin\go2rtc.exe -version

# ffmpeg：錄影、截圖、匯出都需要。使用 gyan.dev 的 essentials 版（GitHub 鏡像，下載較快）
if (-not (Test-Path 'bin\ffmpeg.exe')) {
    Write-Host '下載 ffmpeg（約 110 MB）…'
    $rel = Invoke-RestMethod 'https://api.github.com/repos/GyanD/codexffmpeg/releases/latest' -Headers @{ 'User-Agent' = 'tapo-nvr' }
    $asset = $rel.assets | Where-Object { $_.name -like '*essentials_build.zip' } | Select-Object -First 1
    $zip = Join-Path $env:TEMP $asset.name
    Invoke-WebRequest $asset.browser_download_url -OutFile $zip
    $z = [IO.Compression.ZipFile]::OpenRead($zip)
    try {
        $entry = $z.Entries | Where-Object { $_.FullName -like '*/bin/ffmpeg.exe' } | Select-Object -First 1
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $root 'bin\ffmpeg.exe'), $true)
    } finally { $z.Dispose() }
}
& .\bin\ffmpeg.exe -hide_banner -version | Select-Object -First 1

Write-Host '編譯伺服器…'
cargo build --release -p tapo-nvr
if ($LASTEXITCODE -ne 0) { Write-Host '編譯失敗' -ForegroundColor Red; exit $LASTEXITCODE }

Write-Host ''
Write-Host '完成。' -ForegroundColor Green
Write-Host '  獨立伺服器：雙擊 scripts\start.bat，再用瀏覽器開啟 http://localhost:8080'
Write-Host '  桌面版安裝檔：雙擊 scripts\release.bat（需要 cargo install tauri-cli）'
