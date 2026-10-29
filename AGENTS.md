# AGENTS.md

## 語言（最重要）

- **一律使用繁體中文（台灣用語）回覆使用者**，包含進度說明、問題回報、總結與提問；即使工具輸出、錯誤訊息或程式碼是英文也一樣。
- 程式碼中的識別字維持英文；註解與記錄訊息使用繁體中文；使用者看得到的字串需同時提供英文（伺服器用 `tr!("中文", "English")`，網頁用 `web/i18n.js` 字典）。
- Commit 訊息與 GitHub Release 標題可用英文，Release 說明採中英雙語。

## 專案概要

Tapo NVR：TP-Link Tapo 攝影機的監控與錄影系統。

| 路徑 | 內容 |
|---|---|
| `server/` | Rust 程式庫 `tapo_nvr`（axum）與獨立執行檔 `tapo-nvr` |
| `web/` | 網頁介面（純 JS，release 版以 rust-embed 打包進執行檔） |
| `desktop/` | Tauri 2 桌面版（內建伺服器），`desktop/ui/` 為內建頁面 |
| `scripts/` | 安裝、啟動、發布腳本（`.bat` 呼叫同名 `.ps1`） |
| `docs/` | GitHub Pages 一頁式網站（`docs/index.html`）與文件 |

- 設定存在 SQLite（`data/tapo-nvr.db`），機密以 Windows DPAPI 加密（`server/src/secret.rs`）；**不使用 config.toml 或環境變數保存設定**。
- 桌面版的資料放在安裝資料夾（`data/`、`logs/`、`webview/`），無法寫入時才退回 AppData。
- go2rtc 為附屬程式（設定檔只含 `${TNVR_n}` 佔位符，機密經環境變數傳入），ffmpeg 負責錄影、轉檔、匯出。
- 授權：GNU AGPL-3.0；第三方軟體見 `THIRD_PARTY_NOTICES.md`。

## 建置與測試

```powershell
cargo build -p tapo-nvr -p tapo-nvr-desktop     # 編譯
cargo test -p tapo-nvr                           # 單元測試
node --check web\app.js                          # 前端語法檢查
```

- PowerShell 中 `cargo` 即使成功也常回傳 exit 1（stderr 輸出），請以是否出現 `Finished`、有無 `error` 判斷。
- `server/examples/` 內為診斷用程式（不隨產品發布）。

## 必須遵守

- **不可提交或上傳** `config.toml`、`data/`、`dist/`、`README - 複製.md`，以及任何含攝影機密碼的檔案。
- **不可干擾使用者**：使用者可能正在玩遊戲。不要切換或聚焦視窗、不要擷取全螢幕、不要啟動會彈出視窗的程式。
- **只能依 PID 結束自己啟動的測試程序**，不可依名稱結束程序（使用者安裝的 Tapo NVR 也會執行 `go2rtc.exe`、`tapo-nvr-desktop.exe`）。測試結束要立即關閉測試程序。
- 不可用使用者的 TP-Link 雲端密碼做測試；Tapo 攝影機登入失敗過多會鎖定。
- 使用者的桌面版安裝在 `D:\Tapo NVR`；從 `target\` 執行的開發版不會搬移資料，請勿改掉此保護。

## 編碼注意（Windows PowerShell 5）

- PowerShell 會把 UTF-8 中文弄成亂碼；含中文的檔案請用編輯工具或 Node 腳本修改，不要用 PowerShell 字串取代。
- `.ps1` 腳本需存成 **UTF-8 with BOM**；其他檔案用 UTF-8 無 BOM。
- PowerShell 雙引號 here-string 會展開 `$()` 與反引號，必要時只用單引號 here-string。

## 發布流程

1. 同步修改根目錄 `Cargo.toml` 與 `desktop/src-tauri/tauri.conf.json` 的 `version`。
2. `scripts\release.bat https://github.com/fengnaiouo/Tapo-NVR/releases/download/vX.Y.Z`（產出 `dist\Tapo-NVR_X.Y.Z_x64-setup.exe` 與 `latest.json`）。
3. Commit、push，再 `gh release create vX.Y.Z dist\Tapo-NVR_X.Y.Z_x64-setup.exe dist\latest.json --latest`。
4. 發布後 `.github/workflows/release-linux.yml` 會自動編譯 Linux 伺服器（x86_64／aarch64）、Docker 映像檔（ghcr.io）、桌面版（AppImage／deb），並把 `linux-x86_64` 加進 `latest.json`（簽章金鑰在 GitHub Secrets）。
5. 確認 `releases/latest/download/latest.json` 的版本與平台正確、各安裝檔可下載。
6. 更新簽章私鑰位於 `%USERPROFILE%\.tauri\tapo-nvr.key`（含 `.password`），不可提交、不可遺失。

平台設定：`desktop/src-tauri/tauri.windows.conf.json`、`tauri.linux.conf.json` 分別指定附帶的 go2rtc／ffmpeg 與安裝檔格式。桌面版資料位置：Windows 為安裝資料夾，Linux 為 `~/.local/share/tapo-nvr`。
