# Tapo NVR

[English](README.md) | **繁體中文** | [简体中文](README_CN.md) | [日本語](README_JP.md) | [한국어](README_KO.md)

用 Rust 開發、免費開源的 TP-Link Tapo 攝影機監控與錄影系統，提供 **Windows 桌面應用程式**（內建伺服器）和**網頁版**，兩者功能相同。介面支援繁體中文、简体中文、English（依系統自動選擇）。

🌐 網站：<https://fengnaiouo.github.io/Tapo-NVR/> · ⬇️ [下載最新版](https://github.com/fengnaiouo/Tapo-NVR/releases/latest)

## 功能

| 類別 | 功能 |
|---|---|
| 即時影像 | 多鏡頭格狀畫面、2K 主串流、區網 WebRTC 低延遲、外網 MSE、數位變焦 1–4 倍 |
| 雲台 | 按住方向鍵持續轉動、鍵盤方向鍵、預設點的新增／前往／刪除（ONVIF 或 Tapo API） |
| 攝影機控制 | 隱私模式、夜視、移動偵測與靈敏度、人形偵測、移動追蹤、SD 卡狀態 |
| 錄影 | 每台可選**連續錄影**或**只錄移動事件**（含事件前後畫面）、保留天數與容量上限、磁碟空間不足時自動清理 |
| 回放 | 24 小時時間軸（可縮放到 15 分鐘）、事件標記、0.5–16 倍速、截圖、**匯出 MP4** |
| 偵測與通知 | 移動偵測、自動截圖、事件紀錄；Windows 原生通知、瀏覽器通知、**手機推播**（ntfy）；一鍵**布防／撤防**、**通知時段**（例如只在夜間通知）、**攝影機離線／恢復連線**通知 |
| 雙向語音 | 在電腦上對攝影機說話（需要 TP-Link 帳號密碼） |
| 機型支援 | 自動掃描區網（ONVIF + TP-Link TDP），支援 C1xx / C2xx / C3xx / C5xx 等 Tapo 機型；沒有 RTSP 的機型改用 Tapo 私有串流；同時支援舊版與新版（TPAP）登入協定 |
| 遠端觀看 | 區網直連，或經 Cloudflare Tunnel 從外網連線 |
| 設定 | **所有設定都在應用程式或網頁的 ⚙ 設定頁修改**，大部分立即生效；設定備份與還原（不含密碼）；內建系統記錄檢視；「更新與關於」顯示版本與更新狀態 |
| 桌面版 | 系統匣常駐、開機自動啟動、關閉視窗後背景繼續錄影、應用程式內自動更新 |
| 安全 | 設定存在 SQLite 資料庫，**攝影機密碼以 Windows DPAPI 加密**；密碼不會出現在設定檔或程序指令列；Session 只存雜湊，**關閉瀏覽器／應用程式或重新啟動後需重新登入**；Argon2、登入次數限制、CSRF 防護；攝影機密碼不會傳到瀏覽器 |

## 使用方式

### 1. 設定攝影機（Tapo App）

1. 攝影機 → ⚙ 設定 → **進階設定 → 攝影機帳戶**：建立 RTSP/ONVIF 用的帳號密碼
2. **我 → 第三方服務 → 第三方相容性**：開啟
3. 在路由器把攝影機設為**固定 IP**

### 2a. 桌面應用程式（建議）

1. 從 [Releases](https://github.com/fengnaiouo/Tapo-NVR/releases/latest) 下載並執行 `Tapo-NVR_x.y.z_x64-setup.exe`
2. 第一次開啟：建立管理員帳號
3. **⚙ 設定 → 攝影機**：按「掃描區網攝影機」→「加入」→ 填入攝影機帳戶 →「測試連線」→ 儲存
4. **⚙ 設定 → 錄影**：選擇錄影資料夾（建議空間大的硬碟），再為每台攝影機選擇錄影模式
5. （選用）填入 TP-Link 帳號的密碼，即可使用隱私模式、夜視、雙向語音等控制功能。只需要密碼，攝影機本機登入帳號固定為 `admin`

- 資料位置：`%APPDATA%\com.tapo-nvr.desktop\data\tapo-nvr.db`（設定、事件）與 `snapshots\`；錄影在你選的資料夾
- 從 0.2 版升級時，舊的 `config.toml` 會自動匯入資料庫，並清除其中的密碼
- 系統匣選單：錄影回放、設定、桌面通知開關、開機自動啟動、開啟錄影資料夾、檢查更新
- 「應用程式偏好設定」可以切換成**連線到其他電腦**，只當觀看端

### 2b. 獨立伺服器

```powershell
.\scripts\setup.bat     # 下載 go2rtc / ffmpeg 並編譯
.\scripts\start.bat     # 啟動，瀏覽器開啟 http://localhost:8080
```

設定存在 `data\tapo-nvr.db`；目前資料夾若有舊版 `config.toml` 會自動匯入。

**忘記管理員密碼？** 桌面版：系統匣圖示 →「應用程式偏好設定」→「忘記管理員密碼？」。獨立伺服器：先停止伺服器，再執行 `tapo-nvr reset-password --data data`。

### 其他裝置觀看

同一個區網的手機或電腦，用瀏覽器開啟 `http://<執行 Tapo NVR 的電腦 IP>:8080`。

**Windows 防火牆**（只開放給私人網路）：

```powershell
New-NetFirewallRule -DisplayName "Tapo NVR Web" -Direction Inbound -Protocol TCP -LocalPort 8080 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC" -Direction Inbound -Protocol UDP -LocalPort 8555 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC TCP" -Direction Inbound -Protocol TCP -LocalPort 8555 -Profile Private -Action Allow
```

外網觀看請參考 [docs/cloudflare-tunnel.md](docs/cloudflare-tunnel.md)。

## 更新

桌面版會在啟動 30 秒後及每 6 小時檢查 `releases/latest/download/latest.json`。有新版本時，應用程式視窗右下角會出現更新卡片（含更新說明），按「立即更新」會顯示下載進度，完成後自動安裝並重新啟動。也可以從系統匣選單 →「檢查更新」手動檢查。

## 架構

```
┌──────────────── Tapo NVR.exe（桌面版）────────────────┐
│  視窗（WebView2）＋ 系統匣 ＋ Windows 原生通知          │
│  └─ 內建伺服器（tapo_nvr 程式庫，Rust / axum）          │
│       ├─ SQLite：設定、事件、Session（機密以 DPAPI 加密）│
│       ├─ ONVIF / Tapo API：雲台、預設點、移動事件、控制  │
│       ├─ go2rtc（內附）：RTSP → WebRTC / MSE           │
│       ├─ ffmpeg（內附）：分段錄影、轉檔、匯出、截圖      │
│       └─ 內建網頁介面（同時提供給其他裝置的瀏覽器）      │
└────────────────────────────────────────────────────────┘
```

```
server/   伺服器（Rust 程式庫 tapo_nvr ＋ 獨立執行檔 tapo-nvr）
web/      網頁介面（純 JS，打包進執行檔）
desktop/  Tauri 2 桌面版
scripts/  安裝、啟動、發布腳本
docs/     GitHub Pages 網站與文件
```

## 錄影說明

- 檔案位置：`{錄影資料夾}\{攝影機代號}\{日期}\{時-分-秒}.mp4`，可以直接用一般播放器開啟
- 錄製中的片段是 `.rec.mp4`（當機也不會損壞），完成後自動轉成一般 `.mp4`
- **事件錄影**其實是持續錄製，再自動刪掉沒有移動的片段，所以會保留移動前的畫面
- 2K 連續錄影約每台每天 15–25 GB
- 回放只播放已完成的片段，大約落後即時 30–60 秒

## 發布新版

1. 修改 `desktop/src-tauri/tauri.conf.json` 和根目錄 `Cargo.toml` 的 `version`
2. `scripts\release.bat https://github.com/fengnaiouo/Tapo-NVR/releases/download/vX.Y.Z`
3. `gh release create vX.Y.Z dist\Tapo-NVR_X.Y.Z_x64-setup.exe dist\latest.json`

更新檔以私鑰簽章（`%USERPROFILE%\.tauri\tapo-nvr.key`），**請備份私鑰與密碼檔**，遺失後已安裝的程式將無法再更新。

## 注意事項

- **雙向語音**需要 TP-Link 帳號密碼，並在本機、桌面版或 HTTPS 下使用；經 Cloudflare Tunnel 時無法使用
- **登入保護**：Tapo 攝影機多次密碼錯誤會鎖定，本程式在密碼錯誤時會停止重試
- **瀏覽器通知**需要 HTTPS 或 localhost；桌面版使用 Windows 原生通知
- 本專案與 TP-Link 無關，Tapo 為 TP-Link 的商標
