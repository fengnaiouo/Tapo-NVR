//! 錄影回放 API：日期 / 片段列表、影片串流（支援 Range 拖曳）、匯出片段、狀態。

use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::api::ApiError;
use crate::recorder::{list_day, list_days, valid_date, valid_segment_name};
use crate::AppState;

type ApiResult = Result<Response, ApiError>;

fn root(state: &AppState) -> Result<std::path::PathBuf, ApiError> {
    state
        .cfg()
        .recording
        .root()
        .ok_or_else(|| ApiError::bad(tr!("尚未設定錄影資料夾，請到「設定 → 錄影」選擇", "No recording folder yet. Choose one in Settings → Recording")))
}

fn known_camera(state: &AppState, id: &str) -> Result<(), ApiError> {
    if crate::config::valid_camera_id(id) && state.cfg().cameras.iter().any(|c| c.id == id) {
        Ok(())
    } else {
        Err(ApiError(StatusCode::NOT_FOUND, tr!("找不到攝影機", "Camera not found")))
    }
}

pub async fn status(State(state): State<AppState>) -> Json<Value> {
    let cfg = state.cfg();
    let root = cfg.recording.root();
    let cameras: Vec<Value> = cfg
        .cameras
        .iter()
        .map(|c| {
            let st = state.recorders.status(&c.id);
            json!({"id": c.id, "name": c.name, "mode": c.record, "status": st})
        })
        .collect();
    let (used, free, exists) = match &root {
        Some(r) => {
            let r2 = r.clone();
            let used = tokio::task::spawn_blocking(move || crate::recorder::dir_size(&r2)).await.unwrap_or(0);
            (Some(used), crate::free_space(r), r.exists())
        }
        None => (None, None, false),
    };
    Json(json!({
        "enabled": root.is_some(),
        "path": cfg.recording.path,
        "exists": exists,
        "used_bytes": used,
        "free_bytes": free,
        "cameras": cameras,
    }))
}

pub async fn days(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    known_camera(&state, &id)?;
    let root = root(&state)?;
    let days = tokio::task::spawn_blocking(move || list_days(&root, &id)).await.unwrap_or_default();
    Ok(Json(json!(days)).into_response())
}

#[derive(Deserialize)]
pub struct DateQuery {
    date: String,
}

pub async fn segments(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DateQuery>,
) -> ApiResult {
    known_camera(&state, &id)?;
    if !valid_date(&q.date) {
        return Err(ApiError::bad(tr!("日期格式錯誤", "Invalid date")));
    }
    let root = root(&state)?;
    let (cam, date) = (id.clone(), q.date.clone());
    let segs = tokio::task::spawn_blocking(move || list_day(&root, &cam, &date)).await.unwrap_or_default();
    // 該日（本地時間）的事件，從資料庫查詢，不受記憶體中筆數限制
    let day = chrono::NaiveDate::parse_from_str(&q.date, "%Y-%m-%d").map_err(|_| ApiError::bad(tr!("日期格式錯誤", "Invalid date")))?;
    let local_start = day.and_hms_opt(0, 0, 0).and_then(|t| t.and_local_timezone(Local).earliest());
    let events: Vec<Value> = match local_start {
        Some(start) => {
            let from = start.with_timezone(&chrono::Utc);
            state
                .events
                .events_between(&id, from, from + chrono::Duration::hours(25))
                .into_iter()
                .filter(|e| e.ts.with_timezone(&Local).format("%Y-%m-%d").to_string() == q.date)
                .map(|e| json!({"ts": e.ts, "kind": e.kind, "snapshot": e.snapshot}))
                .collect()
        }
        None => Vec::new(),
    };
    Ok(Json(json!({"segments": segs, "events": events})).into_response())
}

pub async fn video(
    State(state): State<AppState>,
    Path((id, date, file)): Path<(String, String, String)>,
    req: Request,
) -> ApiResult {
    known_camera(&state, &id)?;
    if !valid_date(&date) || !valid_segment_name(&file) {
        return Err(ApiError::bad(tr!("檔名無效", "Invalid file name")));
    }
    let path = root(&state)?.join(&id).join(&date).join(&file);
    // ServeFile 處理 Range / If-Modified-Since，讓播放器可以拖曳
    let resp = tower_http::services::ServeFile::new(&path)
        .oneshot(req)
    .await
    .map_err(|e| ApiError::bad(e.to_string()))?;
    Ok(resp.map(Body::new))
}

#[derive(Deserialize)]
pub struct ExportQuery {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

/// 匯出指定時間範圍：以 ffmpeg concat 合併片段並裁切（不重新編碼）
pub async fn export(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> ApiResult {
    known_camera(&state, &id)?;
    let root = root(&state)?;
    if q.end <= q.start {
        return Err(ApiError::bad(tr!("結束時間需晚於開始時間", "End time must be after start time")));
    }
    if q.end - q.start > chrono::Duration::hours(2) {
        return Err(ApiError::bad(tr!("一次最多匯出 2 小時", "Export is limited to 2 hours")));
    }
    let (start, end) = (q.start.with_timezone(&Local), q.end.with_timezone(&Local));
    let mut dates = vec![start.format("%Y-%m-%d").to_string()];
    let end_date = end.format("%Y-%m-%d").to_string();
    if end_date != dates[0] {
        dates.push(end_date);
    }
    let cam = id.clone();
    let segs: Vec<_> = tokio::task::spawn_blocking(move || {
        dates.iter().flat_map(|d| list_day(&root, &cam, d)).collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default()
    .into_iter()
    .filter(|s| s.end > start && s.start < end)
    .collect();
    let Some(first) = segs.first() else {
        return Err(ApiError(StatusCode::NOT_FOUND, tr!("這段時間沒有錄影", "No recordings in this period")));
    };

    let dir = state.data_dir.join("exports");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| ApiError::bad(e.to_string()))?;
    cleanup_exports(&dir).await;
    let stamp = format!("{}_{}", id, start.format("%Y%m%d_%H%M%S"));
    let list = dir.join(format!("{stamp}_{}.txt", crate::auth::random_hex(4)));
    let out = dir.join(format!("{stamp}_{}.mp4", crate::auth::random_hex(4)));
    let body: String = segs
        .iter()
        .map(|s| format!("file '{}'\n", s.path.to_string_lossy().replace('\'', "'\\''")))
        .collect();
    tokio::fs::write(&list, body).await.map_err(|e| ApiError::bad(e.to_string()))?;

    let offset = (start - first.start).num_milliseconds().max(0) as f64 / 1000.0;
    let duration = (end - start).num_milliseconds() as f64 / 1000.0;
    let mut cmd = tokio::process::Command::new(&state.ffmpeg_bin);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args(["-ss", &format!("{offset:.3}"), "-t", &format!("{duration:.3}")])
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&out)
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let output = tokio::time::timeout(Duration::from_secs(300), cmd.output())
        .await
        .map_err(|_| ApiError::bad(tr!("匯出逾時", "Export timed out")))?
        .map_err(|e| ApiError::bad(tr!("無法執行 ffmpeg：{e}", "Cannot run ffmpeg: {e}")))?;
    let _ = tokio::fs::remove_file(&list).await;
    if !output.status.success() {
        let _ = tokio::fs::remove_file(&out).await;
        return Err(ApiError::bad(tr!("匯出失敗：{}", "Export failed: {}", String::from_utf8_lossy(&output.stderr).trim())));
    }
    let file = tokio::fs::File::open(&out).await.map_err(|e| ApiError::bad(e.to_string()))?;
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let name = format!("{stamp}.mp4");
    Ok((
        [
            (header::CONTENT_TYPE, "video/mp4".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// 背景匯出：建立工作後立即回應，前端輪詢進度，完成後再下載
// （長片段匯出不會讓請求一直等待，經 Cloudflare Tunnel 也不會逾時）

#[derive(Clone, serde::Serialize)]
pub struct ExportJob {
    /// running / done / error
    state: String,
    progress: u8,
    error: Option<String>,
    name: String,
    size: u64,
    #[serde(skip)]
    path: std::path::PathBuf,
    #[serde(skip)]
    owner: String,
    #[serde(skip)]
    created: std::time::Instant,
}

static JOBS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, ExportJob>>> =
    std::sync::LazyLock::new(Default::default);

fn set_job(id: &str, f: impl FnOnce(&mut ExportJob)) {
    if let Some(j) = JOBS.lock().unwrap().get_mut(id) {
        f(j);
    }
}

pub async fn create_export_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::Extension(user): axum::Extension<crate::auth::CurrentUser>,
    Json(q): Json<ExportQuery>,
) -> ApiResult {
    known_camera(&state, &id)?;
    let root = root(&state)?;
    if q.end <= q.start {
        return Err(ApiError::bad(tr!("結束時間需晚於開始時間", "End time must be after start time")));
    }
    if q.end - q.start > chrono::Duration::hours(6) {
        return Err(ApiError::bad(tr!("一次最多匯出 6 小時", "Export is limited to 6 hours")));
    }
    let (start, end) = (q.start.with_timezone(&Local), q.end.with_timezone(&Local));
    let mut dates = vec![start.format("%Y-%m-%d").to_string()];
    let end_date = end.format("%Y-%m-%d").to_string();
    if end_date != dates[0] {
        dates.push(end_date);
    }
    let cam = id.clone();
    let segs: Vec<_> = tokio::task::spawn_blocking(move || dates.iter().flat_map(|d| list_day(&root, &cam, d)).collect::<Vec<_>>())
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.end > start && s.start < end)
        .collect();
    let Some(first) = segs.first() else {
        return Err(ApiError(StatusCode::NOT_FOUND, tr!("這段時間沒有錄影", "No recordings in this period")));
    };
    let dir = state.data_dir.join("exports");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| ApiError::bad(e.to_string()))?;
    cleanup_exports(&dir).await;
    {
        let mut jobs = JOBS.lock().unwrap();
        jobs.retain(|_, j| j.created.elapsed() < Duration::from_secs(3600));
        if jobs.values().filter(|j| j.state == "running").count() >= 3 {
            return Err(ApiError::bad(tr!("同時進行的匯出太多，請稍候再試", "Too many exports in progress, please try again later")));
        }
    }
    let job_id = crate::auth::random_hex(16);
    let stamp = format!("{}_{}", id, start.format("%Y%m%d_%H%M%S"));
    let list = dir.join(format!("{job_id}.txt"));
    let out = dir.join(format!("{job_id}.mp4"));
    let body: String = segs.iter().map(|s| format!("file '{}'\n", s.path.to_string_lossy().replace('\'', "'\\''"))).collect();
    tokio::fs::write(&list, body).await.map_err(|e| ApiError::bad(e.to_string()))?;
    let offset = (start - first.start).num_milliseconds().max(0) as f64 / 1000.0;
    let duration = (end - start).num_milliseconds() as f64 / 1000.0;
    JOBS.lock().unwrap().insert(
        job_id.clone(),
        ExportJob {
            state: "running".into(),
            progress: 0,
            error: None,
            name: format!("{stamp}.mp4"),
            size: 0,
            path: out.clone(),
            owner: user.username.clone(),
            created: std::time::Instant::now(),
        },
    );
    let ffmpeg = state.ffmpeg_bin.clone();
    let jid = job_id.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut cmd = tokio::process::Command::new(&ffmpeg);
        cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-nostats", "-progress", "pipe:1", "-y", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list)
            .args(["-ss", &format!("{offset:.3}"), "-t", &format!("{duration:.3}")])
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(&out)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let result: Result<(), String> = async {
            let mut child = cmd.spawn().map_err(|e| tr!("無法執行 ffmpeg：{e}", "Cannot run ffmpeg: {e}"))?;
            if let Some(stdout) = child.stdout.take() {
                let jid2 = jid.clone();
                tokio::spawn(async move {
                    let mut lines = tokio::io::BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        // out_time_us / out_time_ms 皆為微秒
                        if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
                            if let Ok(us) = v.trim().parse::<f64>() {
                                let pct = ((us / 1e6) / duration.max(0.1) * 100.0).clamp(0.0, 99.0) as u8;
                                set_job(&jid2, |j| j.progress = j.progress.max(pct));
                            }
                        }
                    }
                });
            }
            let mut stderr = String::new();
            if let Some(mut err) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = err.read_to_string(&mut stderr).await;
            }
            let status = tokio::time::timeout(Duration::from_secs(3600), child.wait())
                .await
                .map_err(|_| tr!("匯出逾時", "Export timed out"))?
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(tr!("匯出失敗：{}", "Export failed: {}", stderr.trim()));
            }
            Ok(())
        }
        .await;
        let _ = tokio::fs::remove_file(&list).await;
        match result {
            Ok(()) => {
                let size = tokio::fs::metadata(&out).await.map(|m| m.len()).unwrap_or(0);
                set_job(&jid, |j| {
                    j.state = "done".into();
                    j.progress = 100;
                    j.size = size;
                });
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&out).await;
                tracing::warn!("匯出失敗：{e}");
                set_job(&jid, |j| {
                    j.state = "error".into();
                    j.error = Some(e);
                });
            }
        }
    });
    Ok(Json(json!({"job": job_id})).into_response())
}

fn get_job(id: &str, user: &crate::auth::CurrentUser) -> Result<ExportJob, ApiError> {
    JOBS.lock()
        .unwrap()
        .get(id)
        .filter(|j| j.owner == user.username)
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, tr!("找不到匯出工作（可能已過期）", "Export job not found (it may have expired)")))
}

pub async fn export_job_status(
    Path(job): Path<String>,
    axum::Extension(user): axum::Extension<crate::auth::CurrentUser>,
) -> ApiResult {
    Ok(Json(json!(get_job(&job, &user)?)).into_response())
}

pub async fn export_job_download(
    Path(job): Path<String>,
    axum::Extension(user): axum::Extension<crate::auth::CurrentUser>,
) -> ApiResult {
    let j = get_job(&job, &user)?;
    if j.state != "done" {
        return Err(ApiError::bad(tr!("匯出尚未完成", "The export is not finished yet")));
    }
    let file = tokio::fs::File::open(&j.path).await.map_err(|e| ApiError::bad(e.to_string()))?;
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    Ok((
        [
            (header::CONTENT_TYPE, "video/mp4".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", j.name)),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}

/// 刪除 1 小時前的匯出暫存檔
async fn cleanup_exports(dir: &std::path::Path) {
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else { return };
    while let Ok(Some(e)) = rd.next_entry().await {
        let old = e
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(3600));
        if old {
            let _ = tokio::fs::remove_file(e.path()).await;
        }
    }
}
