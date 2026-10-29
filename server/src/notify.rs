//! ntfy 手機推播（https://ntfy.sh，免費、免註冊；手機安裝 ntfy App 並訂閱同一個主題即可）。

use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::api::ApiError;
use crate::config::NotifyConfig;
use crate::events::Event;
use crate::AppState;

pub fn kind_label(kind: &str) -> String {
    match kind {
        "motion" => tr!("移動偵測", "Motion detected"),
        "person" => tr!("偵測到人形", "Person detected"),
        "tamper" => tr!("鏡頭遭遮蔽", "Camera tampering"),
        "offline" => tr!("攝影機離線", "Camera offline"),
        "online" => tr!("攝影機恢復連線", "Camera back online"),
        other => other.to_string(),
    }
}

async fn send(
    http: &reqwest::Client,
    n: &NotifyConfig,
    title: &str,
    message: &str,
    click: Option<String>,
    image: Option<axum::body::Bytes>,
) -> anyhow::Result<()> {
    let url = format!("{}/{}", n.ntfy_url.trim_end_matches('/'), n.ntfy_topic);
    // 標題與內文放在查詢參數（UTF-8 安全），截圖作為附件本文
    let mut query = vec![("title", title.to_string()), ("message", message.to_string()), ("tags", "rotating_light".into())];
    if let Some(c) = click {
        query.push(("click", c));
    }
    let mut req = http.put(&url).query(&query).timeout(Duration::from_secs(20));
    if !n.ntfy_token.is_empty() {
        req = req.bearer_auth(&n.ntfy_token);
    }
    req = match image {
        Some(jpg) => req.header("Filename", "snapshot.jpg").body(jpg),
        None => req.body(""),
    };
    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{}", tr!("ntfy 回應 {}：{}", "ntfy replied {}: {}", status, body.chars().take(200).collect::<String>()));
    }
    Ok(())
}

pub async fn push(state: &AppState, ev: &Event, jpeg: Option<axum::body::Bytes>) {
    let cfg = state.cfg();
    let n = &cfg.notify;
    if n.ntfy_topic.is_empty() {
        return;
    }
    let title = format!("{}：{}", ev.camera_name, kind_label(&ev.kind));
    let message = ev.ts.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string();
    let click = (!n.public_url.is_empty())
        .then(|| format!("{}/?camera={}", n.public_url.trim_end_matches('/'), ev.camera));
    if let Err(e) = send(&state.http, n, &title, &message, click, jpeg).await {
        tracing::warn!("手機推播失敗：{e}");
    }
}

pub async fn test(State(state): State<AppState>) -> Result<Response, ApiError> {
    let cfg = state.cfg();
    if cfg.notify.ntfy_topic.is_empty() {
        return Err(ApiError::bad(tr!("請先設定並儲存 ntfy 主題", "Set and save an ntfy topic first")));
    }
    send(&state.http, &cfg.notify, &tr!("Tapo NVR 測試通知", "Tapo NVR test notification"), &tr!("手機推播設定成功 🎉", "Push notifications are working 🎉"), None, None)
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?;
    Ok(Json(json!({"ok": true})).into_response())
}
