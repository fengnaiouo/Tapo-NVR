# Tapo NVR

**English** | [繁體中文](README_TW.md) | [简体中文](README_CN.md) | [日本語](README_JP.md) | [한국어](README_KO.md)

A free, open-source monitoring and recording system for TP-Link Tapo cameras, written in Rust. It ships as a **Windows desktop app** (with a built-in server) and a **web UI** with identical features. The UI is available in English, Traditional Chinese and Simplified Chinese (auto-detected).

🌐 Website: <https://fengnaiouo.github.io/Tapo-NVR/> · ⬇️ [Download the latest release](https://github.com/fengnaiouo/Tapo-NVR/releases/latest)

## Features

| Category | Details |
|---|---|
| Live view | Multi-camera grid, 2K main stream, low-latency WebRTC on LAN, MSE over the internet, 1–4× digital zoom |
| PTZ | Press-and-hold movement, keyboard arrows, add / go to / delete presets (ONVIF or Tapo API) |
| Camera controls | Privacy mode, night vision, motion detection & sensitivity, person detection, auto-tracking, SD card status |
| Recording | Per camera: **continuous** or **motion-only** (with pre/post roll); retention by days and size; automatic cleanup when disk space is low |
| Playback | 24-hour timeline (zoomable to 15 minutes), event markers, 0.5–16× speed, snapshots, **MP4 export** |
| Alerts | Motion detection, snapshots, event log; Windows native, browser and **phone push** (ntfy) notifications; one-click **arm / disarm**, **notification schedule** (e.g. nights only), **camera offline / back online** alerts |
| Two-way audio | Talk to the camera from your PC (requires the TP-Link account password) |
| Camera support | LAN discovery (ONVIF + TP-Link TDP); C1xx / C2xx / C3xx / C5xx and other Tapo models; models without RTSP use the Tapo private stream; legacy and new (TPAP) login protocols |
| Remote access | Direct on the LAN, or from anywhere through Cloudflare Tunnel |
| Settings | **Everything is configured in the app or web UI (⚙ Settings)**; most changes apply immediately; settings backup / restore (without passwords); built-in system log viewer; version and update status in Settings → Updates & about |
| Desktop app | Tray icon, start with Windows, keeps recording in the background, in-app auto-update |
| Security | Settings in SQLite with **camera passwords encrypted by Windows DPAPI**; passwords never appear in config files or process command lines; sessions stored as hashes only and **sign-in is required again after closing the browser / app or restarting**; Argon2, login rate limiting, CSRF protection; camera passwords never reach the browser |

## Getting started

### 1. Prepare the camera (Tapo app)

1. Camera → ⚙ Settings → **Advanced Settings → Camera Account**: create a username and password for RTSP/ONVIF
2. **Me → Third-Party Services → Third-Party Compatibility**: turn it on
3. Give the camera a **fixed IP** in your router

### 2a. Desktop app (recommended)

1. Download `Tapo-NVR_x.y.z_x64-setup.exe` from [Releases](https://github.com/fengnaiouo/Tapo-NVR/releases/latest) and run it
2. On first launch, create the administrator account
3. **⚙ Settings → Cameras**: click **Scan LAN** → **Add** → enter the camera account → **Test** → Save
4. **⚙ Settings → Recording**: choose a recording folder (a large drive is recommended), then pick a recording mode per camera
5. (Optional) Enter the TP-Link account password to enable privacy mode, night vision, two-way audio and other controls. Only the password is needed — the camera's local login user is always `admin`

- Data is stored in the install folder (e.g. `D:\Tapo NVR\`): `data\` (settings database, events, snapshots), `logs\`, `webview\`. Recordings go to the folder you choose. Updates keep the data; uninstalling removes it only if you tick "Delete application data"
- If the install folder is not writable (e.g. under `C:\Program Files`), `%APPDATA%\com.tapo-nvr.desktop\` is used instead
- Upgrading from 0.2.x: the old `config.toml` is imported automatically and its passwords are removed
- Tray menu: playback, settings, notifications on/off, start with Windows, open recordings folder, check for updates
- **App preferences** can switch to **Connect to another PC**, using this app as a viewer only

### 2b. Standalone server

```powershell
.\scripts\setup.bat     # download go2rtc / ffmpeg and build
.\scripts\start.bat     # start, then open http://localhost:8080
```

Settings are stored in `data\tapo-nvr.db`; a legacy `config.toml` in the working folder is imported automatically.

**Forgot the admin password?** Desktop app: tray icon → **App preferences** → **Forgot the admin password?**. Standalone server: stop it, then run `tapo-nvr reset-password --data data`.

### Viewing from other devices

On the same LAN, open `http://<IP of the PC running Tapo NVR>:8080` in any browser.

**Windows Firewall** (private networks only):

```powershell
New-NetFirewallRule -DisplayName "Tapo NVR Web" -Direction Inbound -Protocol TCP -LocalPort 8080 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC" -Direction Inbound -Protocol UDP -LocalPort 8555 -Profile Private -Action Allow
New-NetFirewallRule -DisplayName "Tapo NVR WebRTC TCP" -Direction Inbound -Protocol TCP -LocalPort 8555 -Profile Private -Action Allow
```

For access from the internet, see [docs/cloudflare-tunnel.md](docs/cloudflare-tunnel.md) (Traditional Chinese).

## Updates

The desktop app checks `releases/latest/download/latest.json` 30 seconds after launch and every 6 hours. When a new version is available, a card appears in the bottom-right corner of the app window with the release notes. Click **Update now** to download (with a progress bar), install and restart automatically. You can also check manually from the tray menu → **Check for updates**.

## Architecture

```
┌──────────────── Tapo NVR.exe (desktop) ────────────────┐
│  Window (WebView2) + tray + Windows notifications       │
│  └─ Built-in server (tapo_nvr library, Rust / axum)     │
│       ├─ SQLite: settings, events, sessions (DPAPI)     │
│       ├─ ONVIF / Tapo API: PTZ, presets, motion, controls│
│       ├─ go2rtc (bundled): RTSP → WebRTC / MSE          │
│       ├─ ffmpeg (bundled): recording, remux, export     │
│       └─ Web UI (also served to other devices)          │
└─────────────────────────────────────────────────────────┘
```

```
server/   Server (Rust library tapo_nvr + standalone binary tapo-nvr)
web/      Web UI (plain JS, embedded in the binary)
desktop/  Tauri 2 desktop app
scripts/  Setup, start and release scripts
docs/     GitHub Pages website and docs
```

## Recording notes

- Files: `{recording folder}\{camera id}\{date}\{HH-MM-SS}.mp4`, playable in any video player
- Segments being recorded are `.rec.mp4` (crash-safe) and are converted to regular `.mp4` when finished
- **Motion recording** records continuously and deletes segments without motion, so footage before the event is kept
- Continuous 2K recording uses about 15–25 GB per camera per day
- Playback only shows finished segments, about 30–60 seconds behind live

## Releasing a new version

1. Bump `version` in `desktop/src-tauri/tauri.conf.json` and the root `Cargo.toml`
2. `scripts\release.bat https://github.com/fengnaiouo/Tapo-NVR/releases/download/vX.Y.Z`
3. `gh release create vX.Y.Z dist\Tapo-NVR_X.Y.Z_x64-setup.exe dist\latest.json`

Updates are signed with a private key (`%USERPROFILE%\.tauri\tapo-nvr.key`). **Back up the key and its password file** — without them, installed apps can no longer be updated.

## Notes

- **Two-way audio** needs the TP-Link account password and works locally, in the desktop app or over HTTPS; it does not work through Cloudflare Tunnel
- **Lockout protection**: Tapo cameras lock after repeated wrong passwords, so Tapo NVR stops retrying when a password is wrong
- **Browser notifications** need HTTPS or localhost; the desktop app uses native Windows notifications
- This project is not affiliated with TP-Link. Tapo is a trademark of TP-Link.

## License

[GNU AGPL-3.0](LICENSE). If you modify Tapo NVR and offer it to others (including over a network), you must publish your source code under the same license. Bundled programs (go2rtc, FFmpeg) are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
