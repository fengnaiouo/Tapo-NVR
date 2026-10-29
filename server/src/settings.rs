//! 設定 API：所有設定都可從網頁 / 桌面版修改，寫入 SQLite 資料庫並立即套用。

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::ApiError;
use crate::auth::CurrentUser;
use crate::config::{default_onvif_port, default_rtsp_port, valid_camera_id, CameraConfig, RecordMode};
use crate::onvif::OnvifClient;
use crate::AppState;

type ApiResult = Result<Response, ApiError>;

fn ok() -> ApiResult {
    Ok(Json(json!({"ok": true})).into_response())
}

fn bad(e: anyhow::Error) -> ApiError {
    ApiError::bad(e.to_string())
}

#[derive(Serialize)]
struct CameraView {
    id: String,
    name: String,
    host: String,
    username: String,
    has_password: bool,
    rtsp_port: u16,
    onvif_port: u16,
    ptz: bool,
    motion: bool,
    notify: bool,
    record: RecordMode,
    has_cloud_password: bool,
    stream: crate::config::StreamSource,
    model: String,
}

pub async fn get_settings(State(state): State<AppState>) -> Json<Value> {
    let c = state.cfg();
    // 攝影機密碼不回傳給瀏覽器
    let cameras: Vec<CameraView> = c
        .cameras
        .iter()
        .map(|x| CameraView {
            id: x.id.clone(),
            name: x.name.clone(),
            host: x.host.clone(),
            username: x.username.clone(),
            has_password: !x.password.is_empty(),
            rtsp_port: x.rtsp_port,
            onvif_port: x.onvif_port,
            ptz: x.ptz,
            motion: x.motion,
            notify: x.notify,
            record: x.record,
            has_cloud_password: !x.cloud_password.is_empty(),
            stream: x.stream,
            model: x.model.clone(),
        })
        .collect();
    let n = &c.notify;
    Json(json!({
        "cameras": cameras,
        "notify": {
            "enabled": n.enabled,
            "cooldown_secs": n.cooldown_secs,
            "snapshot": n.snapshot,
            "max_events": n.max_events,
            "ntfy_url": n.ntfy_url,
            "ntfy_topic": n.ntfy_topic,
            "has_ntfy_token": !n.ntfy_token.is_empty(),
            "public_url": n.public_url,
            "language": n.language,
            "schedule_enabled": n.schedule_enabled,
            "schedule_start": n.schedule_start,
            "schedule_end": n.schedule_end,
            "schedule_days": n.schedule_days,
            "offline_alert": n.offline_alert,
            "in_schedule": n.in_schedule(chrono::Local::now()),
        },
        "update": c.update,
        "recording": c.recording,
        "ffmpeg": state.ffmpeg_bin.display().to_string(),
        "desktop_embedded": state.desktop_embedded,
        "account": {"username": c.auth.username, "session_days": c.auth.session_days},
        "server": {
            "listen": c.server.listen,
            "trust_proxy_headers": c.server.trust_proxy_headers,
            "webrtc_port": c.go2rtc.webrtc_port,
            "webrtc_candidates": c.go2rtc.webrtc_candidates,
            "api_port": c.go2rtc.api_port,
            "rtsp_port": c.go2rtc.rtsp_port,
        },
        "restart_required": state.restart_required(),
        "can_restart": state.can_restart,
        "config_path": state.store.path.display().to_string(),
    }))
}

#[derive(Deserialize)]
pub struct NotifyReq {
    enabled: bool,
    cooldown_secs: u64,
    snapshot: bool,
    max_events: usize,
    #[serde(default)]
    ntfy_url: String,
    #[serde(default)]
    ntfy_topic: String,
    /// 空字串 = 沿用原本的 token
    #[serde(default)]
    ntfy_token: String,
    #[serde(default)]
    clear_ntfy_token: bool,
    #[serde(default)]
    public_url: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    schedule_enabled: bool,
    #[serde(default)]
    schedule_start: Option<String>,
    #[serde(default)]
    schedule_end: Option<String>,
    #[serde(default)]
    schedule_days: Option<Vec<u8>>,
    #[serde(default)]
    offline_alert: Option<bool>,
}

pub async fn put_notify(State(state): State<AppState>, Json(r): Json<NotifyReq>) -> ApiResult {
    if r.cooldown_secs > 86400 {
        return Err(ApiError::bad(tr!("通知間隔最多 86400 秒", "Notification interval is at most 86400 seconds")));
    }
    if r.max_events > 100_000 {
        return Err(ApiError::bad(tr!("事件保留數量最多 100000", "Keep at most 100000 events")));
    }
    state
        .update_config(|c| {
            c.notify.enabled = r.enabled;
            c.notify.cooldown_secs = r.cooldown_secs;
            c.notify.snapshot = r.snapshot;
            c.notify.max_events = r.max_events;
            c.notify.ntfy_url = if r.ntfy_url.trim().is_empty() { "https://ntfy.sh".into() } else { r.ntfy_url.trim().to_string() };
            c.notify.ntfy_topic = r.ntfy_topic.trim().to_string();
            if r.clear_ntfy_token {
                c.notify.ntfy_token.clear();
            } else if !r.ntfy_token.is_empty() {
                c.notify.ntfy_token = r.ntfy_token.trim().to_string();
            }
            c.notify.public_url = r.public_url.trim().trim_end_matches('/').to_string();
            if let Some(l) = &r.language {
                c.notify.language = match l.as_str() {
                    "zh-TW" | "zh-CN" | "en" => l.clone(),
                    _ => "auto".into(),
                };
            }
            c.notify.schedule_enabled = r.schedule_enabled;
            if let Some(s) = &r.schedule_start {
                c.notify.schedule_start = s.trim().to_string();
            }
            if let Some(s) = &r.schedule_end {
                c.notify.schedule_end = s.trim().to_string();
            }
            if let Some(d) = &r.schedule_days {
                let mut d = d.clone();
                d.sort_unstable();
                d.dedup();
                c.notify.schedule_days = d;
            }
            if let Some(v) = r.offline_alert {
                c.notify.offline_alert = v;
            }
            Ok(())
        })
        .await
        .map_err(bad)?;
    ok()
}

#[derive(Deserialize)]
pub struct ServerReq {
    listen: String,
    trust_proxy_headers: bool,
    webrtc_port: u16,
    #[serde(default)]
    webrtc_candidates: Vec<String>,
    session_days: i64,
    #[serde(default)]
    api_port: Option<u16>,
    #[serde(default)]
    rtsp_port: Option<u16>,
}

#[derive(Deserialize)]
pub struct ArmReq {
    armed: bool,
}

/// 首頁的布防 / 撤防開關（通知總開關）
pub async fn put_armed(State(state): State<AppState>, Json(r): Json<ArmReq>) -> ApiResult {
    state
        .update_config(|c| {
            c.notify.enabled = r.armed;
            Ok(())
        })
        .await
        .map_err(bad)?;
    tracing::info!("{}", if r.armed { "已布防（開啟通知）" } else { "已撤防（暫停通知）" });
    ok()
}

#[derive(Deserialize)]
pub struct UpdateReq {
    auto_check: bool,
}

pub async fn put_update(State(state): State<AppState>, Json(r): Json<UpdateReq>) -> ApiResult {
    state
        .update_config(|c| {
            c.update.auto_check = r.auto_check;
            Ok(())
        })
        .await
        .map_err(bad)?;
    ok()
}

pub async fn put_server(State(state): State<AppState>, Json(r): Json<ServerReq>) -> ApiResult {
    state
        .update_config(|c| {
            c.server.listen = r.listen.trim().to_string();
            c.server.trust_proxy_headers = r.trust_proxy_headers;
            c.go2rtc.webrtc_port = r.webrtc_port;
            c.go2rtc.webrtc_candidates = r
                .webrtc_candidates
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            c.auth.session_days = r.session_days;
            if let Some(p) = r.api_port {
                c.go2rtc.api_port = p;
            }
            if let Some(p) = r.rtsp_port {
                c.go2rtc.rtsp_port = p;
            }
            Ok(())
        })
        .await
        .map_err(bad)?;
    Ok(Json(json!({"ok": true, "restart_required": state.restart_required()})).into_response())
}

#[derive(Deserialize)]
pub struct AccountReq {
    current_password: String,
    username: String,
    #[serde(default)]
    new_password: String,
}

pub async fn put_account(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(r): Json<AccountReq>,
) -> ApiResult {
    let hash = state.cfg().auth.password_hash.clone();
    let current = r.current_password.clone();
    let valid = tokio::task::spawn_blocking(move || crate::auth::verify_password(&current, &hash))
        .await
        .unwrap_or(false);
    if !valid {
        return Err(ApiError(StatusCode::FORBIDDEN, tr!("目前密碼錯誤", "Current password is wrong")));
    }
    let username = r.username.trim().to_string();
    if username.is_empty() || username.chars().count() > 32 {
        return Err(ApiError::bad(tr!("帳號需為 1–32 字", "Username must be 1–32 characters")));
    }
    let new_hash = if r.new_password.is_empty() {
        None
    } else {
        if r.new_password.chars().count() < 8 {
            return Err(ApiError::bad(tr!("新密碼至少 8 個字元", "New password must be at least 8 characters")));
        }
        let pw = r.new_password.clone();
        Some(
            tokio::task::spawn_blocking(move || crate::auth::hash_password(&pw))
                .await
                .map_err(|e| ApiError::bad(e.to_string()))?
                .map_err(bad)?,
        )
    };
    let password_changed = new_hash.is_some();
    let u = username.clone();
    state
        .update_config(move |c| {
            c.auth.username = u;
            if let Some(h) = new_hash {
                c.auth.password_hash = h;
            }
            Ok(())
        })
        .await
        .map_err(bad)?;
    state.auth.rename_user(&username).await;
    if password_changed {
        state.auth.revoke_others(&user.token).await;
        tracing::info!("管理員密碼已變更，其他裝置已登出");
    }
    Ok(Json(json!({"ok": true, "others_logged_out": password_changed})).into_response())
}

#[derive(Deserialize, Clone)]
pub struct CameraInput {
    #[serde(default)]
    id: String,
    name: String,
    host: String,
    username: String,
    /// 空字串 = 沿用原本的密碼
    #[serde(default)]
    password: String,
    rtsp_port: Option<u16>,
    onvif_port: Option<u16>,
    ptz: Option<bool>,
    motion: Option<bool>,
    notify: Option<bool>,
    record: Option<RecordMode>,
    /// 空字串 = 沿用原本的雲端密碼
    #[serde(default)]
    cloud_password: String,
    #[serde(default)]
    clear_cloud_password: bool,
    stream: Option<crate::config::StreamSource>,
    #[serde(default)]
    model: Option<String>,
}

impl CameraInput {
    fn into_config(self, id: String, existing: Option<&CameraConfig>) -> CameraConfig {
        let password = if self.password.is_empty() {
            existing.map(|c| c.password.clone()).unwrap_or_default()
        } else {
            self.password
        };
        let cloud_password = if self.clear_cloud_password {
            String::new()
        } else if self.cloud_password.is_empty() {
            existing.map(|c| c.cloud_password.clone()).unwrap_or_default()
        } else {
            self.cloud_password
        };
        CameraConfig {
            id,
            name: self.name.trim().to_string(),
            host: self.host.trim().to_string(),
            username: self.username.trim().to_string(),
            password,
            rtsp_port: self.rtsp_port.unwrap_or_else(default_rtsp_port),
            onvif_port: self.onvif_port.unwrap_or_else(default_onvif_port),
            ptz: self.ptz.unwrap_or(true),
            motion: self.motion.unwrap_or(true),
            notify: self.notify.unwrap_or(true),
            record: self.record.unwrap_or_default(),
            cloud_password,
            stream: self.stream.unwrap_or_default(),
            model: self.model.map(|m| m.trim().chars().take(32).collect()).filter(|m: &String| !m.is_empty())
                .or_else(|| existing.map(|c| c.model.clone()))
                .unwrap_or_default(),
        }
    }
}

/// 自動產生代號：cam1、cam2…
fn next_camera_id(existing: &[CameraConfig]) -> String {
    (1..)
        .map(|n| format!("cam{n}"))
        .find(|id| !existing.iter().any(|c| &c.id == id))
        .unwrap()
}

pub async fn create_camera(State(state): State<AppState>, Json(r): Json<CameraInput>) -> ApiResult {
    if r.password.is_empty() && r.cloud_password.is_empty() {
        return Err(ApiError::bad(tr!("請輸入攝影機帳戶密碼", "Enter the camera account password")));
    }
    let mut new_id = String::new();
    state
        .update_config(|c| {
            let id = if r.id.trim().is_empty() { next_camera_id(&c.cameras) } else { r.id.trim().to_string() };
            if !valid_camera_id(&id) {
                anyhow::bail!("{}", tr!("代號只能包含小寫英數字、- 或 _（最多 32 字）", "ID may only contain lowercase letters, digits, - or _ (max 32)"));
            }
            if c.cameras.iter().any(|x| x.id == id) {
                anyhow::bail!("{}", tr!("代號 `{id}` 已存在", "ID `{id}` already exists"));
            }
            new_id = id.clone();
            c.cameras.push(r.into_config(id, None));
            Ok(())
        })
        .await
        .map_err(bad)?;
    tracing::info!("新增攝影機 {new_id}");
    Ok(Json(json!({"ok": true, "id": new_id})).into_response())
}

pub async fn update_camera(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(r): Json<CameraInput>,
) -> ApiResult {
    state
        .update_config(|c| {
            let Some(slot) = c.cameras.iter_mut().find(|x| x.id == id) else {
                anyhow::bail!("{}", tr!("找不到攝影機 `{id}`", "Camera `{id}` not found"));
            };
            let updated = r.into_config(id.clone(), Some(&*slot));
            *slot = updated;
            Ok(())
        })
        .await
        .map_err(bad)?;
    ok()
}

pub async fn delete_camera(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    state
        .update_config(|c| {
            let before = c.cameras.len();
            c.cameras.retain(|x| x.id != id);
            if c.cameras.len() == before {
                anyhow::bail!("{}", tr!("找不到攝影機 `{id}`", "Camera `{id}` not found"));
            }
            Ok(())
        })
        .await
        .map_err(bad)?;
    tracing::info!("刪除攝影機 {id}");
    ok()
}

#[derive(Deserialize)]
pub struct OrderReq {
    ids: Vec<String>,
}

pub async fn reorder_cameras(State(state): State<AppState>, Json(r): Json<OrderReq>) -> ApiResult {
    state
        .update_config(|c| {
            let mut sorted = c.cameras.clone();
            sorted.sort_by_key(|x| r.ids.iter().position(|i| i == &x.id).unwrap_or(usize::MAX));
            c.cameras = sorted;
            Ok(())
        })
        .await
        .map_err(bad)?;
    ok()
}

#[derive(Serialize)]
pub struct TestReport {
    pub ok: bool,
    pub lines: Vec<String>,
    pub ptz: Option<bool>,
    /// 偵測到的機型（Tapo 控制 API 或 ONVIF）
    pub model: Option<String>,
}

/// 依序檢查：RTSP 連接埠 → ONVIF 登入 → PTZ → 事件訂閱
pub async fn test_camera(cam: &CameraConfig) -> TestReport {
    let mut lines = Vec::new();
    let mut ok = true;
    let mut model = None;
    let tapo_stream = cam.stream == crate::config::StreamSource::Tapo;

    let addr = format!("{}:{}", cam.host, cam.rtsp_port);
    if tapo_stream {
        lines.push(tr!("ℹ️ 使用 Tapo 私有串流（不需要 RTSP）", "ℹ️ Using the Tapo stream (RTSP not required)"));
    } else {
    match tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => lines.push(tr!("✅ RTSP 連接埠 {} 可連線", "✅ RTSP port {} is reachable", cam.rtsp_port)),
        _ => {
            ok = false;
            lines.push(tr!("❌ 無法連線 RTSP {addr}（請確認 IP、攝影機已開機、與伺服器在同一網段）", "❌ Cannot reach RTSP {addr} (check the IP, that the camera is on, and on the same network)"));
        }
    }
    }

    let client = OnvifClient::new(&cam.host, cam.onvif_port, &cam.username, &cam.password);
    let mut ptz = None;
    match client.capabilities().await {
        Ok((has_ptz, has_events)) => {
            lines.push(tr!("✅ ONVIF 登入成功（攝影機帳戶正確）", "✅ ONVIF login OK (camera account is correct)"));
            ptz = Some(has_ptz);
            if has_ptz {
                match client.presets().await {
                    Ok(p) => lines.push(tr!("✅ 雲台可用，預設點 {} 個", "✅ Pan/tilt available, {} presets", p.len())),
                    Err(e) => lines.push(tr!("⚠️ 雲台回應異常：{e}", "⚠️ Pan/tilt error: {e}")),
                }
            } else {
                lines.push(tr!("ℹ️ 此攝影機沒有雲台", "ℹ️ This camera has no pan/tilt"));
            }
            if has_events {
                match client.subscribe().await {
                    Ok(sub) => {
                        client.unsubscribe(&sub).await;
                        lines.push(tr!("✅ 移動偵測事件可訂閱", "✅ Motion events available"));
                    }
                    Err(e) => lines.push(tr!("⚠️ 事件訂閱失敗：{e}", "⚠️ Event subscription failed: {e}")),
                }
            } else {
                lines.push(tr!("⚠️ 攝影機未提供 ONVIF 事件服務", "⚠️ The camera provides no ONVIF event service"));
            }
        }
        Err(e) if tapo_stream || (cam.password.is_empty() && !cam.cloud_password.is_empty()) => {
            lines.push(tr!(
                "ℹ️ ONVIF 無法使用（{e}）：雲台與預設點改用 Tapo 控制 API；移動偵測通知需要 ONVIF",
                "ℹ️ ONVIF unavailable ({e}): pan/tilt and presets use the Tapo API; motion notifications need ONVIF"
            ));
        }
        Err(e) => {
            ok = false;
            let msg = e.to_string();
            let hint = if msg.contains("Authorized") || msg.contains("401") || msg.contains("400") {
                tr!("帳號或密碼錯誤：請使用 Tapo App →「攝影機帳戶」的帳密，而非 TP-Link 雲端帳號", "Wrong username or password: use the Tapo app → Camera Account credentials, not your TP-Link cloud account")
            } else if msg.contains("無法連線") || msg.contains("Cannot connect") {
                tr!("連不上 ONVIF：請到 Tapo App → 我 → 第三方服務 → 開啟「第三方相容性」", "ONVIF unreachable: in the Tapo app go to Me → Third-Party Services and enable Third-Party Compatibility")
            } else {
                tr!("ONVIF 回應異常", "ONVIF error")
            };
            lines.push(format!("❌ {hint}（{msg}）"));
        }
    }
    // 只有填了雲端密碼才測試（每次登入都會計入攝影機的嘗試次數）
    if !cam.cloud_password.is_empty() {
        let client = crate::tapo::TapoClient::new(&cam.host, &cam.cloud_password);
        match client.status(true).await {
            Ok(s) => {
                model = s["model"].as_str().map(str::to_string);
                lines.push(tr!(
                "✅ 攝影機控制可用（{} 韌體 {}）",
                "✅ Camera controls available ({} firmware {})",
                s["model"].as_str().unwrap_or("Tapo"),
                s["firmware"].as_str().unwrap_or("?")
            ));
            }
            Err(e) => {
                ok = false;
                lines.push(tr!("❌ 攝影機控制：{e}", "❌ Camera controls: {e}"));
            }
        }
    }
    TestReport { ok, lines, ptz, model }
}

pub async fn test_camera_handler(State(state): State<AppState>, Json(r): Json<CameraInput>) -> ApiResult {
    let id = r.id.trim().to_string();
    let existing = state.camera(&id).map(|c| c.cfg.clone());
    let cam = r.into_config(if id.is_empty() { "test".into() } else { id }, existing.as_ref());
    if cam.password.is_empty() && cam.cloud_password.is_empty() {
        return Err(ApiError::bad(tr!("請輸入攝影機帳戶密碼", "Enter the camera account password")));
    }
    if cam.host.is_empty() {
        return Err(ApiError::bad(tr!("請輸入攝影機 IP", "Enter the camera IP")));
    }
    Ok(Json(test_camera(&cam).await).into_response())
}

pub async fn discover(State(state): State<AppState>) -> Json<Value> {
    let found = crate::discovery::probe(Duration::from_secs(3)).await;
    let cfg = state.cfg();
    let list: Vec<Value> = found
        .into_iter()
        .map(|f| {
            let configured = cfg.cameras.iter().find(|c| c.host == f.host).map(|c| c.name.clone());
            let mut v = json!(f);
            v["configured_as"] = json!(configured);
            v
        })
        .collect();
    Json(json!(list))
}

pub async fn restart(State(state): State<AppState>) -> ApiResult {
    tracing::info!("收到重新啟動要求");
    let s = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        s.request_restart();
    });
    ok()
}

#[derive(Deserialize)]
pub struct RecordingReq {
    path: String,
    max_days: u32,
    max_gb: u64,
    pre_seconds: u64,
    post_seconds: u64,
    segment_seconds: u64,
}

pub async fn put_recording(State(state): State<AppState>, Json(r): Json<RecordingReq>) -> ApiResult {
    let path = r.path.trim().to_string();
    if !path.is_empty() {
        let p = std::path::Path::new(&path);
        if !p.is_absolute() {
            return Err(ApiError::bad(tr!("請選擇完整的資料夾路徑", "Choose a full folder path")));
        }
        std::fs::create_dir_all(p).map_err(|e| ApiError::bad(tr!("無法建立資料夾：{e}", "Cannot create folder: {e}")))?;
        // 確認可寫入
        let probe = p.join(".tapo-nvr-write-test");
        std::fs::write(&probe, b"ok").map_err(|e| ApiError::bad(tr!("資料夾無法寫入：{e}", "Folder is not writable: {e}")))?;
        let _ = std::fs::remove_file(probe);
    }
    state
        .update_config(|c| {
            c.recording.path = path;
            c.recording.max_days = r.max_days;
            c.recording.max_gb = r.max_gb;
            c.recording.pre_seconds = r.pre_seconds;
            c.recording.post_seconds = r.post_seconds;
            c.recording.segment_seconds = r.segment_seconds;
            Ok(())
        })
        .await
        .map_err(bad)?;
    ok()
}

#[derive(Deserialize)]
pub struct BrowseQuery {
    #[serde(default)]
    path: String,
}

/// 伺服器端資料夾瀏覽（網頁與桌面版共用的「選擇資料夾」）
pub async fn browse(Query(q): Query<BrowseQuery>) -> ApiResult {
    let path = q.path.trim();
    if path.is_empty() {
        #[cfg(windows)]
        let roots: Vec<String> = (b'A'..=b'Z')
            .map(|d| format!("{}:\\", d as char))
            .filter(|d| std::path::Path::new(d).exists())
            .collect();
        #[cfg(not(windows))]
        let roots = vec!["/".to_string()];
        let drives: Vec<Value> = roots
            .into_iter()
            .map(|d| json!({"name": d, "path": d, "free_bytes": crate::free_space(std::path::Path::new(&d))}))
            .collect();
        return Ok(Json(json!({"path": "", "parent": null, "dirs": drives})).into_response());
    }
    let p = std::path::PathBuf::from(path);
    if !p.is_absolute() || !p.is_dir() {
        return Err(ApiError::bad(tr!("資料夾不存在", "Folder does not exist")));
    }
    let mut dirs: Vec<Value> = std::fs::read_dir(&p)
        .map_err(|e| ApiError::bad(tr!("無法讀取：{e}", "Cannot read: {e}")))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('$') || name.starts_with('.') || name == "System Volume Information" {
                return None;
            }
            Some(json!({"name": name, "path": e.path().to_string_lossy()}))
        })
        .collect();
    dirs.sort_by(|a, b| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()));
    let parent = p.parent().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    Ok(Json(json!({
        "path": p.to_string_lossy(),
        "parent": parent,
        "dirs": dirs,
        "free_bytes": crate::free_space(&p),
    }))
    .into_response())
}

#[derive(Deserialize)]
pub struct MkdirReq {
    parent: String,
    name: String,
}

pub async fn mkdir(Json(r): Json<MkdirReq>) -> ApiResult {
    let name = r.name.trim();
    if name.is_empty() || name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) || name == ".." {
        return Err(ApiError::bad(tr!("資料夾名稱無效", "Invalid folder name")));
    }
    let parent = std::path::PathBuf::from(r.parent.trim());
    if !parent.is_absolute() || !parent.is_dir() {
        return Err(ApiError::bad(tr!("上層資料夾不存在", "Parent folder does not exist")));
    }
    let p = parent.join(name);
    std::fs::create_dir_all(&p).map_err(|e| ApiError::bad(tr!("無法建立：{e}", "Cannot create: {e}")))?;
    Ok(Json(json!({"path": p.to_string_lossy()})).into_response())
}