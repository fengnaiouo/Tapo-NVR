# Tapo NVR

**繁體中文** | [English](#english)

用 Rust 開發的 TP-Link Tapo 攝影機監控與錄影系統，提供 **Windows 桌面應用程式**（內建伺服器）和**網頁版**，兩者功能相同。介面支援繁體中文、简体中文、English（依系統自動選擇）。

🌐 網站：<https://fengnaiouo.github.io/Tapo-NVR/> ・ ⬇️ [下載最新版](https://github.com/fengnaiouo/Tapo-NVR/releases/latest)

## 功能

| 類別 | 功能 |
|---|---|
| 即時影像 | 多鏡頭格狀畫面、2K 主串流放大檢視、WebRTC 低延遲（區網）、MSE（外網）、數位變焦 1–4 倍 |
| 雲台 | 按住方向鍵持續轉動、鍵盤方向鍵、預設點的新增／前往／刪除（ONVIF 或 Tapo API） |
| 攝影機控制 | 隱私模式、夜視、移動偵測與靈敏度、人形偵測、移動追蹤、SD 卡狀態 |
| 錄影 | 每台可選**連續錄影**或**只錄移動事件**（含事件前後畫面）、保留天數與容量上限、磁碟空間不足時自動清理 |
| 回放 | 24 小時時間軸（可縮放到 15 分鐘）、事件標記、0.5–16 倍速、截圖、**匯出片段**（MP4） |
| 偵測與通知 | 移動偵測、自動截圖、事件紀錄；Windows 原生通知、瀏覽器通知、**手機推播**（ntfy） |
| 雙向語音 | 在電腦上對攝影機說話（需要 TP-Link 雲端密碼） |
| 機型支援 | 自動掃描區網（ONVIF + TP-Link TDP），支援 C1xx / C2xx / C3xx / C5xx 等 Tapo 機型；沒有 RTSP 的機型改用 Tapo 私有串流；同時支援舊版與新版（TPAP）登入協定 |
| 遠端觀看 | 區網直連，或經 Cloudflare Tunnel 從外網連線 |
| 設定 | **所有設定都在應用程式或網頁的 ⚙ 設定頁修改**，大部分立即生效 |
| 桌面版 | 系統匣常駐、開機自動啟動、關閉視窗後背景繼續錄影和通知、自動更新 |
| 安全 | 設定存在 SQLite 資料庫，**攝影機密碼以 Windows DPAPI 加密**；Session 只存雜湊；Argon2 密碼雜湊、登入次數限制、CSRF 防護；攝影機密碼不會傳到瀏覽器，也不會寫入任何設定檔 |

## 使用方式

### 先設定攝影機（Tapo App）

1. 攝影機 → ⚙ 設定 → **進階設定 → 攝影機帳戶**：建立 RTSP/ONVIF 用的帳號密碼
2. **我 → 第三方服務 → 第三方相容性**：開啟
3. 在路由器把攝影機設為**固定 IP**

### 方式 A：桌面應用程式（建議）

1. 從 [Releases](https://github.com/fengnaiouo/Tapo-NVR/releases/latest) 下載並執行 `Tapo NVR_x.y.z_x64-setup.exe`
2. 第一次開啟：建立管理員帳號
3. **⚙ 設定 → 攝影機**：按「🔍 掃描區網攝影機」→「加入」→ 填入攝影機帳戶 →「🔌 測試連線」→ 儲存
4. **⚙ 設定 → 錄影**：選擇錄影資料夾（建議空間大的硬碟），再到每台攝影機選擇錄影模式
5. （選用）填入 TP-Link 雲端密碼即可使用隱私模式、夜視、雙向語音等控制功能

- 資料位置：`%APPDATA%\com.tapo-nvr.desktop\data\tapo-nvr.db`（設定、事件）與 `snapshots\`；錄影在你選的資料夾
- 從 0.2 版升級時，舊的 `config.toml` 會自動匯入資料庫，並清除其中的密碼
- 系統匣右鍵選單：錄影回放、設定、桌面通知開關、開機自動啟動、開啟錄影資料夾、檢查更新
- 「應用程式偏好設定」可以切換成**連線到其他電腦**的模式，只當觀看端

### 方式 B：獨立伺服器

```powershell
.\scripts\setup.bat     # 下載 go2rtc / ffmpeg 並編譯
.\scripts\start.bat     # 啟動，瀏覽器開啟 http://localhost:8080
```

設定存在 `data\tapo-nvr.db`；目前資料夾若有舊版 `config.toml` 會自動匯入。

### 其他裝置觀看

同一個區網的手機或電腦，用瀏覽器開啟 `http://<執行 Tapo NVR 的電腦 IP>:8080`。

**Windows 防火牆**（只開放給私人網路）：

```powershell
New-NetFirewallRule -DisplayName "Tapo NVR Web" -Direction Inbound -Protocol TCP -LocalPort 8080 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC" -Direction Inbound -Protocol UDP -LocalPort 8555 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC TCP" -Direction Inbound -Protocol TCP -LocalPort 8555 -Profile Private -Action Allow
```

外網觀看請參考 [docs/cloudflare-tunnel.md](docs/cloudflare-tunnel.md)。

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
3. `gh release create vX.Y.Z dist\*.exe dist\latest.json`

已安裝的程式會從 `releases/latest/download/latest.json` 自動檢查更新。更新檔以私鑰簽章（`%USERPROFILE%\.tauri\tapo-nvr.key`），**請備份私鑰與密碼檔**。

## 注意事項

- **雙向語音**需要 TP-Link 雲端密碼，並在本機、桌面版或 HTTPS 下使用；經 Cloudflare Tunnel 時無法使用
- **登入保護**：Tapo 攝影機多次密碼錯誤會鎖定，本程式在密碼錯誤時會停止重試
- **瀏覽器通知**需要 HTTPS 或 localhost；桌面版用 Windows 原生通知
- 本專案與 TP-Link 無關

---

<a id="english"></a>

# Tapo NVR (English)

A monitoring and recording system for TP-Link Tapo cameras, written in Rust. Ships as a **Windows desktop app** (with a built-in server) and a **web UI** with identical features. UI languages: Traditional Chinese, Simplified Chinese and English (auto-detected).

🌐 Website: <https://fengnaiouo.github.io/Tapo-NVR/> ・ ⬇️ [Download](https://github.com/fengnaiouo/Tapo-NVR/releases/latest)

## Features

- **Live view**: multi-camera grid, 2K main stream, low-latency WebRTC on LAN, MSE over the internet, digital zoom
- **PTZ**: press-and-hold movement, keyboard arrows, presets (ONVIF or Tapo API)
- **Camera controls**: privacy mode, night vision, motion detection & sensitivity, person detection, auto-tracking, SD card
- **Recording**: continuous or motion-only (with pre/post roll), retention by days and size, automatic cleanup
- **Playback**: 24-hour timeline, event markers, 0.5–16× speed, snapshots, MP4 export
- **Notifications**: Windows native, browser, and phone push via ntfy (with snapshots)
- **Two-way audio** (requires the TP-Link cloud password)
- **All Tapo models**: LAN discovery via ONVIF + TP-Link TDP; models without RTSP use the Tapo private stream; both legacy and new (TPAP) login protocols
- **Security**: settings in SQLite with **camera passwords encrypted by Windows DPAPI**; sessions stored as hashes; Argon2, rate limiting, CSRF protection
- **Desktop**: tray icon, start with Windows, background recording, auto-update

## Getting started

1. In the Tapo app: create a **Camera Account** (Advanced Settings) and enable **Third-Party Compatibility**; give the camera a fixed IP
2. Download and run `Tapo NVR_x.y.z_x64-setup.exe` from [Releases](https://github.com/fengnaiouo/Tapo-NVR/releases/latest)
3. Create the admin account, then **⚙ Settings → Cameras → Scan LAN** → Add → Test → Save
4. **⚙ Settings → Recording**: choose a recording folder

Standalone server: `scripts\setup.bat` then `scripts\start.bat`, and open <http://localhost:8080>. Settings are stored in `data\tapo-nvr.db`; a legacy `config.toml` in the working folder is imported automatically and its secrets are removed.

This project is not affiliated with TP-Link.
