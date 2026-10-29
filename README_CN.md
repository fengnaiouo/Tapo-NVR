# Tapo NVR

[English](README.md) | [繁體中文](README_TW.md) | **简体中文** | [日本語](README_JP.md) | [한국어](README_KO.md)

用 Rust 开发、免费开源的 TP-Link Tapo 摄像头监控与录像系统，提供 **Windows 桌面应用程序**（内置服务器）和**网页版**，两者功能相同。界面支持繁体中文、简体中文、English（依系统自动选择）。

🌐 网站：<https://fengnaiouo.github.io/Tapo-NVR/> · ⬇️ [下载最新版](https://github.com/fengnaiouo/Tapo-NVR/releases/latest)

## 功能

| 类别 | 功能 |
|---|---|
| 实时画面 | 多镜头格状画面、2K 主码流、局域网 WebRTC 低延迟、外网 MSE、数字变焦 1–4 倍 |
| 云台 | 按住方向键持续转动、键盘方向键、预置点的添加／前往／删除（ONVIF 或 Tapo API） |
| 摄像头控制 | 隐私模式、夜视、移动侦测与灵敏度、人形侦测、移动追踪、SD 卡状态 |
| 录像 | 每台可选**连续录像**或**只录移动事件**（含事件前后画面）、**录像调度**（例如上班时间不录、夜间连续录像）、保留天数与容量上限、磁盘空间不足时自动清理 |
| 回放 | 24 小时时间轴（可缩放到 15 分钟）、事件标记、0.5–16 倍速、截屏、**导出 MP4（最长 6 小时，背景处理并显示进度）** |
| 侦测与通知 | 移动侦测与 **Tapo 智能侦测**（人形、车辆、宠物、婴儿哭声、越线、区域入侵），每台可选通知类型；自动截屏、事件记录；Windows 原生通知、浏览器通知、**手机推送**（ntfy）；一键**布防／撤防**、**通知时段**（例如只在夜间通知）、**摄像头脱机／恢复连接**通知 |
| 双向语音 | 在电脑上对摄像头说话（需要 TP-Link 账号密码） |
| 机型支持 | 自动扫描局域网（ONVIF + TP-Link TDP），也可扫描其他网段；**摄像头 IP 变动时自动找回**（以 MAC 比对）；连接测试会说明原因（IP 已变更、NAT、账号错误）；支持 C1xx / C2xx / C3xx / C5xx 等 Tapo 机型；没有 RTSP 的机型改用 Tapo 私有视频流；同时支持旧版与新版（TPAP）登录协议 |
| 远程观看 | 局域网直连，或经 Cloudflare Tunnel 从外网连接 |
| 用户 | **多用户**：管理员，以及只能观看、回放、导出但不能改设置的「仅观看」账号 |
| 设置 | **所有设置都在应用程序或网页的 ⚙ 设置页修改**，大部分立即生效；**系统状态总览**（连接、移动侦测、录像、空间与问题提醒）；设置备份与还原（不含密码）；内置系统记录查看；「更新与关于」显示版本与更新状态 |
| 桌面版 | 系统托盘常驻、开机自动启动、关闭窗口后后台继续录像、应用程序内自动更新 |
| 安全 | 设置存在 SQLite 数据库，**摄像头密码以 Windows DPAPI 加密**；密码不会出现在设置档或进程指令列；Session 只存哈希，**关闭浏览器／应用程序或重新启动后需重新登录**；Argon2、登录次数限制、CSRF 防护；摄像头密码不会传到浏览器 |

## 使用方式

### 1. 设置摄像头（Tapo App）

1. 摄像头 → ⚙ 设置 → **高级设置 → 摄像头账户**：创建 RTSP/ONVIF 用的账号密码
2. **我 → 第三方服务 → 第三方兼容性**：打开
3. 在路由器把摄像头设为**固定 IP**

### 2a. 桌面应用程序（推荐）

1. 从 [Releases](https://github.com/fengnaiouo/Tapo-NVR/releases/latest) 下载并运行 `Tapo-NVR_x.y.z_x64-setup.exe`
2. 第一次打开：创建管理员账号
3. **⚙ 设置 → 摄像头**：按「扫描局域网摄像头」→「加入」→ 填入摄像头账户 →「测试连接」→ 保存
4. **⚙ 设置 → 录像**：选择录像文件夹（建议空间大的硬盘），再为每台摄像头选择录像模式
5. （可选）填入 TP-Link 账号的密码，即可使用隐私模式、夜视、双向语音等控制功能。只需要密码，摄像头本机登录账号固定为 `admin`

- 数据都存在安装文件夹（例如 `D:\Tapo NVR\`）：`data\`（设置数据库、事件、截屏）、`logs\`、`webview\`；录像在你选的文件夹。更新时数据会保留；卸载时勾选「删除应用程序数据」才会删除
- 安装文件夹无法写入时（例如装在 `C:\Program Files`），改用 `%APPDATA%\com.tapo-nvr.desktop\`
- 从 0.2 版升级时，旧的 `config.toml` 会自动导入数据库，并清除其中的密码
- 系统托盘菜单：录像回放、设置、桌面通知开关、开机自动启动、打开录像文件夹、检查更新
- 「应用程序偏好设置」可以切换成**连接到其他电脑**，只当观看端

### 2b. 独立服务器

```powershell
.\scripts\setup.bat     # 下載 go2rtc / ffmpeg 並編譯
.\scripts\start.bat     # 啟動，瀏覽器開啟 http://localhost:8080
```

设置存在 `data\tapo-nvr.db`；目前文件夹若有旧版 `config.toml` 会自动导入。

**忘记管理员密码？** 桌面版：系统托盘图标 →「应用程序偏好设置」→「忘记管理员密码？」。独立服务器：先停止服务器，再运行 `tapo-nvr reset-password --data data`。

### 2c. Linux

从 [Releases](https://github.com/fengnaiouo/Tapo-NVR/releases/latest) 下载：

| 文件 | 适用 | 安装方式 |
|---|---|---|
| `tapo-nvr-X.Y.Z-linux-x86_64.tar.gz`／`-linux-aarch64.tar.gz` | 服务器、小主机、树莓派（64 比特） | `tar xzf tapo-nvr-*.tar.gz && cd tapo-nvr-*/ && sudo ./install.sh`：安装到 `/opt/tapo-nvr`，数据放在 `/var/lib/tapo-nvr`，并创建开机自动启动的 systemd 服务 |
| Docker `ghcr.io/fengnaiouo/tapo-nvr` | NAS（Synology、Unraid、TrueNAS）或任何 Docker 主机（amd64／arm64） | 见下方 |
| `Tapo-NVR_X.Y.Z_amd64.AppImage`／`.deb` | Linux 桌面（有系统托盘图标） | AppImage：`chmod +x` 后直接运行（支持自动更新）；deb：`sudo apt install ./Tapo-NVR_*.deb` |

**Docker**（`docker/docker-compose.yml`）：

```yaml
services:
  tapo-nvr:
    image: ghcr.io/fengnaiouo/tapo-nvr:latest
    restart: unless-stopped
    network_mode: host        # 掃描攝影機與 WebRTC 需要
    environment:
      - TZ=Asia/Taipei
    volumes:
      - ./data:/data
      - ./recordings:/recordings   # 在「設定 → 錄影」把錄影資料夾設為 /recordings
```

Linux 注意事项：
- 摄像头密码改用文件夹内的密钥档（`secret.key`，权限 600）加密，而不是 Windows DPAPI，请保护好文件夹。
- `tapo-nvr` 服务账号需要能写入录像文件夹，例如 `sudo chown tapo-nvr:tapo-nvr /srv/recordings`。
- 部分发行版的桌面版内嵌浏览器不支持 WebRTC，会自动改用 MSE（延迟稍高）。

### 其他设备观看

同一个局域网的手机或电脑，用浏览器打开 `http://<运行 Tapo NVR 的电脑 IP>:8080`。

**Windows 防火墙**（只开放给私人网络）：

```powershell
New-NetFirewallRule -DisplayName "Tapo NVR Web" -Direction Inbound -Protocol TCP -LocalPort 8080 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC" -Direction Inbound -Protocol UDP -LocalPort 8555 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC TCP" -Direction Inbound -Protocol TCP -LocalPort 8555 -Profile Private -Action Allow
```

外网观看请参考 [docs/cloudflare-tunnel.md](docs/cloudflare-tunnel.md)。

## 更新

桌面版会在启动 30 秒后及每 6 小时检查 `releases/latest/download/latest.json`。有新版本时，应用程序窗口右下角会出现更新卡片（含更新说明），按「立即更新」会显示下载进度，完成后自动安装并重新启动。也可以从系统托盘菜单 →「检查更新」手动检查。

## 架构

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

## 录像说明

- 文件位置：`{录像文件夹}\{摄像头代号}\{日期}\{时-分-秒}.mp4`，可以直接用一般播放器打开
- 录制中的片段是 `.rec.mp4`（当机也不会损坏），完成后自动转成一般 `.mp4`
- **事件录像**其实是持续录制，再自动删掉没有移动的片段，所以会保留移动前的画面
- 2K 连续录像约每台每天 15–25 GB
- 回放只播放已完成的片段，大约落后实时 30–60 秒

## 发布新版

1. 修改 `desktop/src-tauri/tauri.conf.json` 和根目录 `Cargo.toml` 的 `version`
2. `scripts\release.bat https://github.com/fengnaiouo/Tapo-NVR/releases/download/vX.Y.Z`
3. `gh release create vX.Y.Z dist\Tapo-NVR_X.Y.Z_x64-setup.exe dist\latest.json`

更新包以私钥签名（`%USERPROFILE%\.tauri\tapo-nvr.key`），**请备份私钥与密码文件**，遗失后已安装的程序将无法再更新。

## 注意事项

- **双向语音**需要 TP-Link 账号密码，并在本机、桌面版或 HTTPS 下使用；经 Cloudflare Tunnel 时无法使用
- **登录保护**：Tapo 摄像头多次密码错误会锁定，本程序在密码错误时会停止重试
- **浏览器通知**需要 HTTPS 或 localhost；桌面版使用 Windows 原生通知
- 本项目与 TP-Link 无关，Tapo 为 TP-Link 的商标

## 许可证

[GNU AGPL-3.0](LICENSE)。修改 Tapo NVR 并提供给他人使用（包含透过网络提供服务）时，必须以相同授权公开源代码。安装档附带的程序（go2rtc、FFmpeg）请见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
