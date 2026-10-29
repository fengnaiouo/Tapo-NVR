# Tapo NVR：TP-Link Tapo 攝影機監控與錄影

用 Rust 開發的 Tapo C210 監控系統，提供 **Windows 桌面應用程式**（內建伺服器）和**網頁版**，兩者功能相同。

## 功能

| 類別 | 功能 |
|---|---|
| 即時影像 | 多鏡頭格狀畫面、2K 主串流放大檢視、WebRTC 低延遲（區網）、MSE（外網）、數位變焦 1–4 倍 |
| 雲台 | 按住方向鍵持續轉動、鍵盤方向鍵、預設點的新增／前往／刪除 |
| 錄影 | 每台可選**連續錄影**或**只錄移動事件**（含事件前後畫面）、可設定保留天數和容量上限、磁碟空間不足時自動清理 |
| 回放 | 24 小時時間軸（可縮放到 15 分鐘）、事件標記、0.5–16 倍速、截圖、**匯出片段**（MP4） |
| 偵測與通知 | 攝影機內建移動偵測（ONVIF）、自動截圖、事件紀錄 |
| 通知管道 | Windows 原生通知（附截圖，點擊可開啟該攝影機）、瀏覽器通知、**手機推播**（ntfy，附截圖） |
| 雙向語音 | 在電腦上對攝影機說話（需要 TP-Link 雲端密碼） |
| 遠端觀看 | 區網直連，或經 Cloudflare Tunnel 從外網連線 |
| 設定 | **所有設定都能在應用程式或網頁的 ⚙ 設定頁修改**，大部分立即生效 |
| 桌面版 | 系統匣常駐、開機自動啟動、關閉視窗後背景繼續錄影和通知、自動更新 |
| 安全 | Argon2 密碼雜湊、登入次數限制、CSRF 防護、攝影機密碼不會傳到瀏覽器、首次設定限區網 |

## C210 支援的協定（已查證）

- **RTSP**（554）：`stream1` 是 2K 主串流，`stream2` 是子串流
- **ONVIF Profile S**（2020）：影像、PTZ、移動事件。不支援 Profile T，所以雙向語音要走 Tapo 私有協定
- 沒有官方雲端 API 或 API key；沒有光學變焦
- Tapo Care、SD 卡錄影、ONVIF/NVR 三者**同時只能啟用兩項**

## 架構

```
┌──────────────── Tapo NVR.exe（桌面版）────────────────┐
│  視窗（WebView2）＋ 系統匣 ＋ Windows 原生通知          │
│  └─ 內建伺服器（tapo_nvr 程式庫，Rust / axum）          │
│       ├─ ONVIF：PTZ、預設點、移動事件訂閱               │
│       ├─ go2rtc（內附）：RTSP → WebRTC / MSE           │
│       ├─ ffmpeg（內附）：分段錄影、轉檔、匯出、截圖      │
│       ├─ 事件紀錄、ntfy 推播、錄影清理                  │
│       └─ 內建網頁介面（同時提供給其他裝置的瀏覽器）      │
└────────────────────────────────────────────────────────┘
        ↑ 區網：http://<這台電腦>:8080   外網：Cloudflare Tunnel
     手機／筆電瀏覽器，或另一台電腦的桌面版（遠端模式）
```

```
server/            伺服器（Rust 程式庫 tapo_nvr ＋ 獨立執行檔 tapo-nvr）
  src/lib.rs        啟動流程、設定熱套用
  src/recorder.rs   錄影、片段轉檔、清理（事件模式／天數／容量／磁碟空間）
  src/recordings.rs 回放 API（列表、Range 串流、匯出）
  src/events.rs     移動偵測、事件、截圖
  src/notify.rs     ntfy 手機推播
  src/onvif.rs      ONVIF 用戶端
  src/go2rtc.rs     go2rtc 管理與受限代理
  src/settings.rs   設定 API、資料夾瀏覽、攝影機測試
  src/discovery.rs  區網攝影機掃描
web/               網頁介面（純 JS，release 版打包進執行檔）
desktop/           Tauri 2 桌面版
scripts/           安裝、啟動、發布腳本（.bat 啟動對應的 .ps1）
```

## 使用方式

### 先設定攝影機（Tapo App）

1. 攝影機 → ⚙ 設定 → **進階設定 → 攝影機帳戶**：建立 RTSP/ONVIF 用的帳號密碼
2. **我 → 第三方服務 → 第三方相容性**：開啟
3. 在路由器把攝影機設為**固定 IP**

### 方式 A：桌面應用程式（建議）

1. 執行安裝檔 `Tapo NVR_x.y.z_x64-setup.exe`
2. 第一次開啟：建立管理員帳號
3. **⚙ 設定 → 攝影機**：按「🔍 掃描區網攝影機」→「加入」→ 填入攝影機帳戶 →「🔌 測試連線」→ 儲存
4. **⚙ 設定 → 錄影**：選擇錄影資料夾（建議空間大的硬碟），再到每台攝影機選擇錄影模式
5. （選用）**⚙ 設定 → 偵測與通知**：設定 ntfy 手機推播

- 先前用 `start.bat` 執行過的話，可以從系統匣 →「應用程式偏好設定」→「匯入現有 config.toml」
- 資料位置：`%APPDATA%\com.tapo-nvr.desktop\`（設定檔、事件、截圖）；錄影在你選的資料夾
- 系統匣右鍵選單：錄影回放、設定、桌面通知開關、開機自動啟動、開啟錄影資料夾、檢查更新
- 「應用程式偏好設定」可以切換成**連線到其他電腦**的模式，只當觀看端

### 方式 B：獨立伺服器（例如放在不常操作的主機）

```powershell
.\scripts\setup.bat     # 下載 go2rtc / ffmpeg 並編譯
.\scripts\start.bat     # 啟動，瀏覽器開啟 http://localhost:8080
```

### 其他裝置觀看

同一個區網的手機或電腦，用瀏覽器開啟 `http://<執行 Tapo NVR 的電腦 IP>:8080`。

**Windows 防火牆**（只開放給私人網路）：

```powershell
New-NetFirewallRule -DisplayName "Tapo NVR Web" -Direction Inbound -Protocol TCP -LocalPort 8080 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC" -Direction Inbound -Protocol UDP -LocalPort 8555 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC TCP" -Direction Inbound -Protocol TCP -LocalPort 8555 -Profile Private -Action Allow
```

外網觀看請參考 [docs/cloudflare-tunnel.md](docs/cloudflare-tunnel.md)。

## 錄影說明

- 檔案位置：`{錄影資料夾}\{攝影機代號}\{日期}\{時-分-秒}.mp4`，可以直接用一般播放器開啟
- 錄製中的片段是 `.rec.mp4`（當機也不會損壞），完成後約 20 秒內自動轉成一般 `.mp4`
- **事件錄影**其實是持續錄製，再自動刪掉沒有移動的片段，所以會保留移動前的畫面
- C210 2K 連續錄影約每台每天 15–25 GB；事件錄影視活動量而定
- 回放只播放已完成的片段，大約落後即時 30–60 秒；即時畫面請用監控頁

## 發布新版與自動更新

1. 修改 `desktop/src-tauri/tauri.conf.json` 和根目錄 `Cargo.toml` 的 `version`
2. 執行 `scripts\release.bat <下載網址>`，例如：
   ```
   scripts\release.bat https://github.com/你的帳號/tapo-nvr/releases/download/v0.3.0 "新增 xx 功能"
   ```
3. 把 `dist\` 內的安裝檔和 `latest.json` 上傳到該網址
4. 在應用程式的「偏好設定 → 更新來源網址」填入 `latest.json` 的網址（只需設定一次）

更新檔用私鑰簽章，私鑰在 `%USERPROFILE%\.tauri\tapo-nvr.key`（密碼在同目錄的 `.password` 檔）。
**請備份這兩個檔案，遺失後已安裝的程式將無法再收到更新。**

## 注意事項

- **雙向語音**：要在攝影機設定填入 TP-Link 雲端帳號密碼，並且在本機、桌面版或 HTTPS 下使用（瀏覽器限制麥克風權限）；經 Cloudflare Tunnel 時因 WebRTC 無法穿透，無法使用
- **雲台持續轉動**：攝影機韌體不支援時，會自動改用每次轉動 10%
- **瀏覽器通知**需要 HTTPS 或 localhost；桌面版用 Windows 原生通知，不受此限制
- 從網頁修改設定時會重新寫入 `config.toml`，檔案裡的註解會消失
# Tapo-NVR
