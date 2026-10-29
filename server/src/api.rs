use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{AppState, CameraHandle};

type ApiResult = Result<Response, ApiError>;

pub struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        ApiError(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::BAD_GATEWAY, e.to_string())
    }
}

fn camera(state: &AppState, id: &str) -> Result<Arc<CameraHandle>, ApiError> {
    state
        .camera(id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, tr!("找不到攝影機", "Camera not found")))
}

fn ptz_camera(state: &AppState, id: &str) -> Result<Arc<CameraHandle>, ApiError> {
    let cam = camera(state, id)?;
    if !cam.cfg.ptz {
        return Err(ApiError(StatusCode::BAD_REQUEST, tr!("此攝影機未啟用雲台", "Pan/tilt is not enabled for this camera")));
    }
    Ok(cam)
}

pub async fn list_cameras(State(state): State<AppState>) -> Json<Value> {
    let mut out = Vec::new();
    for cam in &state.cfg().cameras {
        let status = state.events.status(&cam.id).await;
        out.push(json!({
            "id": cam.id,
            "name": cam.name,
            "ptz": cam.ptz,
            "motion": cam.motion,
            "main_stream": cam.main_stream(),
            "sub_stream": cam.sub_stream(),
            "talk_stream": cam.talk_stream(),
            "controls": !cam.cloud_password.is_empty(),
            "record": cam.record,
            "recording": state.recorders.status(&cam.id),
            "status": status,
        }));
    }
    Json(json!(out))
}

#[derive(Deserialize)]
pub struct MoveReq {
    pan: f32,
    tilt: f32,
    #[serde(default)]
    mode: Option<String>,
}

/// 一步相對移動：優先 ONVIF，失敗時（例如機型沒有 ONVIF）改用 Tapo 控制 API
async fn step_move(cam: &CameraHandle, pan: f32, tilt: f32) -> anyhow::Result<()> {
    match cam.onvif.relative_move(pan * 0.1, tilt * 0.1).await {
        Ok(()) => Ok(()),
        Err(e) => match &cam.tapo {
            Some(t) => t.motor_move(pan, tilt).await,
            None => Err(e),
        },
    }
}

pub async fn ptz_move(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MoveReq>,
) -> ApiResult {
    let cam = ptz_camera(&state, &id)?;
    if req.mode.as_deref() == Some("step") {
        step_move(&cam, req.pan, req.tilt).await?;
        return Ok(Json(json!({"mode": "step"})).into_response());
    }
    match cam.onvif.continuous_move(req.pan, req.tilt).await {
        Ok(()) => {
            // 前端按住時每 700ms 重送一次；若連線中斷，1.5 秒後自動停止
            let gen = cam.ptz_gen.fetch_add(1, Ordering::SeqCst) + 1;
            let onvif = cam.onvif.clone();
            let counter = cam.ptz_gen.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                if counter.load(Ordering::SeqCst) == gen {
                    let _ = onvif.stop().await;
                }
            });
            Ok(Json(json!({"mode": "continuous"})).into_response())
        }
        Err(e) => {
            tracing::info!("[{id}] ContinuousMove 失敗（{e}），改用逐步移動");
            step_move(&cam, req.pan, req.tilt).await?;
            Ok(Json(json!({"mode": "step"})).into_response())
        }
    }
}

pub async fn ptz_stop(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let cam = ptz_camera(&state, &id)?;
    cam.ptz_gen.fetch_add(1, Ordering::SeqCst);
    // 逐步移動（Tapo API）不需要停止指令
    if let Err(e) = cam.onvif.stop().await {
        if cam.tapo.is_none() {
            return Err(e.into());
        }
    }
    Ok(Json(json!({"ok": true})).into_response())
}

pub async fn list_presets(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let cam = ptz_camera(&state, &id)?;
    let presets = match cam.onvif.presets().await {
        Ok(p) => p,
        Err(e) => match &cam.tapo {
            Some(t) => t.presets().await?,
            None => return Err(e.into()),
        },
    };
    Ok(Json(json!(presets)).into_response())
}

#[derive(Deserialize)]
pub struct PresetReq {
    name: String,
}

pub async fn save_preset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PresetReq>,
) -> ApiResult {
    let cam = ptz_camera(&state, &id)?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 32 {
        return Err(ApiError(StatusCode::BAD_REQUEST, tr!("名稱需為 1–32 字", "Name must be 1–32 characters")));
    }
    let token = match cam.onvif.set_preset(name).await {
        Ok(t) => t,
        Err(e) => match &cam.tapo {
            Some(t) => {
                t.save_preset(name).await?;
                String::new()
            }
            None => return Err(e.into()),
        },
    };
    Ok(Json(json!({"token": token})).into_response())
}

pub async fn goto_preset(
    State(state): State<AppState>,
    Path((id, token)): Path<(String, String)>,
) -> ApiResult {
    let cam = ptz_camera(&state, &id)?;
    if let Err(e) = cam.onvif.goto_preset(&token).await {
        match &cam.tapo {
            Some(t) => t.goto_preset(&token).await?,
            None => return Err(e.into()),
        }
    }
    Ok(Json(json!({"ok": true})).into_response())
}

pub async fn delete_preset(
    State(state): State<AppState>,
    Path((id, token)): Path<(String, String)>,
) -> ApiResult {
    let cam = ptz_camera(&state, &id)?;
    if let Err(e) = cam.onvif.remove_preset(&token).await {
        match &cam.tapo {
            Some(t) => t.delete_preset(&token).await?,
            None => return Err(e.into()),
        }
    }
    Ok(Json(json!({"ok": true})).into_response())
}

pub async fn snapshot(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let cam = camera(&state, &id)?;
    let jpg = crate::go2rtc::snapshot(&state, &cam.cfg.main_stream()).await?;
    let filename = format!(
        "attachment; filename=\"{}_{}.jpg\"",
        cam.cfg.id,
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg".to_string()),
            (header::CONTENT_DISPOSITION, filename),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        jpg,
    )
        .into_response())
}

fn tapo_client(state: &AppState, id: &str) -> Result<Arc<crate::tapo::TapoClient>, ApiError> {
    camera(state, id)?.tapo.clone().ok_or_else(|| {
        ApiError::bad(tr!("尚未設定 TP-Link 雲端密碼：請到「設定 → 攝影機 → 編輯 → 攝影機控制與雙向語音」填入", "TP-Link cloud password not set: go to Settings → Cameras → Edit → Camera controls & two-way audio"))
    })
}

#[derive(Deserialize)]
pub struct DeviceQuery {
    #[serde(default)]
    refresh: bool,
}

/// 攝影機本身的設定（隱私模式、夜視、偵測、追蹤、SD 卡）
pub async fn device_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DeviceQuery>,
) -> ApiResult {
    let client = tapo_client(&state, &id)?;
    Ok(Json(client.status(q.refresh).await?).into_response())
}

#[derive(Deserialize)]
pub struct DeviceUpdate {
    privacy: Option<bool>,
    night_vision: Option<String>,
    motion_detection: Option<bool>,
    motion_sensitivity: Option<String>,
    person_detection: Option<bool>,
    auto_track: Option<bool>,
}

pub async fn device_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(r): Json<DeviceUpdate>,
) -> ApiResult {
    let client = tapo_client(&state, &id)?;
    if let Some(v) = r.privacy {
        client.set_privacy(v).await?;
        tracing::info!("[{id}] 隱私模式：{}", if v { "開啟" } else { "關閉" });
    }
    if let Some(v) = &r.night_vision {
        client.set_night_vision(v).await?;
    }
    if r.motion_detection.is_some() || r.motion_sensitivity.is_some() {
        client.set_motion_detection(r.motion_detection, r.motion_sensitivity.as_deref()).await?;
    }
    if let Some(v) = r.person_detection {
        client.set_person_detection(v).await?;
    }
    if let Some(v) = r.auto_track {
        client.set_auto_track(v).await?;
    }
    client.invalidate().await;
    let status = client.status(true).await?;
    state.events.broadcast(json!({"type": "device", "camera": id, "device": status}));
    Ok(Json(status).into_response())
}

#[derive(Deserialize)]
pub struct EventsQuery {
    limit: Option<usize>,
    camera: Option<String>,
}

pub async fn list_events(State(state): State<AppState>, Query(q): Query<EventsQuery>) -> Json<Value> {
    let limit = q.limit.unwrap_or(100).min(1000);
    Json(json!(state.events.recent(limit, q.camera.as_deref()).await))
}

pub async fn event_snapshot(State(state): State<AppState>, Path(file): Path<String>) -> ApiResult {
    let valid = file.ends_with(".jpg")
        && file
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        && !file.contains("..");
    if !valid {
        return Err(ApiError(StatusCode::BAD_REQUEST, tr!("檔名無效", "Invalid file name")));
    }
    match tokio::fs::read(state.events.snapshot_dir.join(&file)).await {
        Ok(bytes) => Ok((
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "private, max-age=86400"),
            ],
            bytes,
        )
            .into_response()),
        Err(_) => Err(ApiError(StatusCode::NOT_FOUND, tr!("找不到截圖", "Snapshot not found"))),
    }
}
