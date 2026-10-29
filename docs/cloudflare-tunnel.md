# 外網觀看：Cloudflare Tunnel

不需要在路由器開任何 port。cloudflared 從家裡主動連出到 Cloudflare，外部只能透過 HTTPS 網址存取。

```
手機 / 筆電 ──HTTPS──> Cloudflare ──Tunnel──> cloudflared（家中主機）──> http://localhost:8080
```

## 前置需求

- 一個託管在 Cloudflare 的網域（例如 `example.com`）
- 安裝 cloudflared：`winget install Cloudflare.cloudflared`

## 1. 建立 Tunnel

```powershell
cloudflared tunnel login
cloudflared tunnel create tapo-nvr
cloudflared tunnel route dns tapo-nvr cam.example.com
```

## 2. 設定檔 `%USERPROFILE%\.cloudflared\config.yml`

```yaml
tunnel: tapo-nvr
credentials-file: C:\Users\<你>\.cloudflared\<TUNNEL-ID>.json

ingress:
  - hostname: cam.example.com
    service: http://localhost:8080
  - service: http_status:404
```

## 3. 修改 Tapo NVR 設定

到 **⚙ 設定 → 伺服器**：

- 勾選「**透過 Cloudflare Tunnel 對外**」：信任 CF-Connecting-IP（登入次數限制）與 X-Forwarded-Proto（Secure Cookie）
- 「監聽位址」改成 `127.0.0.1:8080` 只允許本機（cloudflared）連線；若區網也要直連，保持 `0.0.0.0:8080`
- 儲存後重新啟動伺服器

> ⚠️ 勾選「透過 Cloudflare Tunnel 對外」時，區網使用者可以偽造來源 IP 標頭。若 `listen` 仍是 `0.0.0.0`，請確保區網是可信任的。

## 4. 安裝成 Windows 服務（開機自動啟動）

```powershell
cloudflared service install
```

## 5. 建議的安全強化

1. **Cloudflare Access**（Zero Trust → Access → Applications）：替 `cam.example.com` 加上 Email OTP 或 Google 登入，成為第二道驗證。
   - 桌面版背景通知是由程式直接連線，無法通過 Access 的互動式登入。如果要使用桌面版通知，請為 `/api/*` 建立 Service Token 或 Bypass 規則，或改用區網／VPN 連線。
2. **WAF 速率限制**：對 `/api/login` 設定每分鐘 10 次。
3. 使用強密碼（至少 8 字元，建議 16 字元以上）。

## 串流模式說明

Cloudflare Tunnel 只轉送 TCP（HTTP／WebSocket），**WebRTC 的 UDP 無法通過**。網頁會自動偵測：

| 連線方式 | 自動選擇的模式 | 延遲 |
|---|---|---|
| 區網 IP／localhost | WebRTC，失敗時改用 MSE | 約 0.3 秒 |
| 外部網址（Tunnel） | MSE（經由 WebSocket） | 約 1–2 秒 |

可以在頁面右上角的下拉選單手動切換模式。
