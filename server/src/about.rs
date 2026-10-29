//! 關於與更新：版本、檢查 GitHub 上的新版本、系統記錄、設定備份與還原。

use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::ApiError;
use crate::config::Config;
use crate::AppState;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_UPDATE_URL: &str = "https://github.com/fengnaiouo/Tapo-NVR/releases/latest/download/latest.json";
pub const RELEASES_URL: &str = "https://github.com/fengnaiouo/Tapo-NVR/releases/latest";
pub const HOMEPAGE_URL: &str = "https://fengnaiouo.github.io/Tapo-NVR/";
pub const REPO_URL: &str = "https://github.com/fengnaiouo/Tapo-NVR";

type ApiResult = Result<Response, ApiError>;

#[derive(Clone, Default, Serialize)]
struct UpdateInfo {
    latest: Option<String>,
    notes: Option<String>,
    pub_date: Option<String>,
    checked_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

static INFO: Mutex<UpdateInfo> = Mutex::new(UpdateInfo { latest: None, notes: None, pub_date: None, checked_at: None, error: None });

/// "v0.3.1" / "0.3.1" → (0, 3, 1)
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(a), Some(b)) if a > b)
}

async fn check_now(state: &AppState) {
    let result: anyhow::Result<(String, Option<String>, Option<String>)> = async {
        let resp = state
            .http
            .get(DEFAULT_UPDATE_URL)
            .header(header::USER_AGENT, format!("tapo-nvr/{VERSION}"))
            .timeout(Duration::from_secs(20))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {}", resp.status());
        }
        let j: Value = resp.json().await?;
        let v = j["version"].as_str().ok_or_else(|| anyhow::anyhow!("latest.json: no version"))?.to_string();
        Ok((v, j["notes"].as_str().map(str::to_string), j["pub_date"].as_str().map(str::to_string)))
    }
    .await;
    let mut info = INFO.lock().unwrap();
    info.checked_at = Some(Utc::now());
    match result {
        Ok((v, notes, date)) => {
            if is_newer(&v, VERSION) {
                tracing::info!("發現新版本 {v}（目前 {VERSION}）");
            }
            info.latest = Some(v.trim_start_matches('v').to_string());
            info.notes = notes.filter(|n| !n.trim().is_empty());
            info.pub_date = date;
            info.error = None;
        }
        Err(e) => {
            tracing::warn!("檢查更新失敗：{e}");
            info.error = Some(tr!("檢查更新失敗：{e}", "Update check failed: {e}"));
        }
    }
}

/// 背景：啟動 20 秒後檢查一次，之後每 6 小時（可在設定關閉）
pub async fn update_loop(state: AppState) {
    let mut wait = Duration::from_secs(20);
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => return,
            _ = tokio::time::sleep(wait) => {}
        }
        if state.cfg().update.auto_check {
            check_now(&state).await;
        }
        wait = Duration::from_secs(6 * 3600);
    }
}

fn about_json(state: &AppState) -> Value {
    let info = INFO.lock().unwrap().clone();
    let available = info.latest.as_deref().is_some_and(|l| is_newer(l, VERSION));
    json!({
        "version": VERSION,
        "desktop": state.desktop_embedded,
        "auto_check": state.cfg().update.auto_check,
        "update": info,
        "update_available": available,
        "releases_url": RELEASES_URL,
        "homepage_url": HOMEPAGE_URL,
        "repo_url": REPO_URL,
        "data_path": state.store.path.display().to_string(),
    })
}

pub async fn about(State(state): State<AppState>) -> Json<Value> {
    Json(about_json(&state))
}

pub async fn check(State(state): State<AppState>) -> Json<Value> {
    check_now(&state).await;
    Json(about_json(&state))
}

// ----------------------------------------------------------------------
// 系統記錄

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    level: Option<String>,
}

pub async fn logs(Query(q): Query<LogQuery>) -> Json<Value> {
    let lines = crate::logbuf::lines(q.limit.unwrap_or(500).min(2000), q.level.as_deref() == Some("warn"));
    Json(json!({"lines": lines}))
}

// ----------------------------------------------------------------------
// 設定備份與還原（不含任何密碼）

pub async fn backup(State(state): State<AppState>) -> Response {
    let mut cfg = (*state.cfg()).clone();
    cfg.auth.password_hash.clear();
    cfg.auth.users.clear();
    cfg.notify.ntfy_token.clear();
    for c in &mut cfg.cameras {
        c.password.clear();
        c.cloud_password.clear();
    }
    let body = json!({
        "app": "tapo-nvr",
        "version": VERSION,
        "exported_at": Utc::now(),
        "note": "Passwords are not included / 不含任何密碼",
        "config": cfg,
    });
    let name = format!("tapo-nvr-settings-{}.json", chrono::Local::now().format("%Y%m%d-%H%M"));
    (
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        serde_json::to_string_pretty(&body).unwrap_or_default(),
    )
        .into_response()
}

/// 還原：攝影機、錄影、通知、伺服器設定以備份為準；管理員帳密不變；
/// 同代號攝影機沿用目前已儲存的密碼，ntfy 權杖也沿用
pub async fn restore(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult {
    if body["app"] != "tapo-nvr" {
        return Err(ApiError::bad(tr!("這不是 Tapo NVR 的設定備份檔", "This is not a Tapo NVR settings backup")));
    }
    let incoming: Config = serde_json::from_value(body["config"].clone())
        .map_err(|e| ApiError::bad(tr!("備份檔格式錯誤：{e}", "Invalid backup file: {e}")))?;
    let count = incoming.cameras.len();
    state
        .update_config(move |c| {
            let old = c.clone();
            let mut next = incoming;
            next.auth = old.auth.clone();
            next.server.data_dir = old.server.data_dir.clone();
            next.server.web_dir = old.server.web_dir.clone();
            next.go2rtc.binary = old.go2rtc.binary.clone();
            next.notify.ntfy_token = old.notify.ntfy_token.clone();
            for cam in &mut next.cameras {
                if let Some(prev) = old.cameras.iter().find(|p| p.id == cam.id) {
                    if cam.password.is_empty() {
                        cam.password = prev.password.clone();
                    }
                    if cam.cloud_password.is_empty() {
                        cam.cloud_password = prev.cloud_password.clone();
                    }
                }
            }
            *c = next;
            Ok(())
        })
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?;
    tracing::info!("已從備份還原設定（{count} 台攝影機）");
    Ok(Json(json!({"ok": true, "cameras": count, "restart_required": state.restart_required()})).into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::is_newer("0.3.2", "0.3.1"));
        assert!(super::is_newer("v1.0.0", "0.9.9"));
        assert!(!super::is_newer("0.3.1", "0.3.1"));
        assert!(!super::is_newer("0.2.9", "0.3.0"));
    }
}
