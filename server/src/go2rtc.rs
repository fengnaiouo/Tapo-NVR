//! go2rtc 附屬程式：產生設定、程序監控、受限的 WebSocket / 檔案代理。

use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use axum::extract::ws::{Message as AMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message as TMsg;

use crate::config::Config;
use crate::AppState;

/// 把設定中的機密（串流網址內的攝影機密碼、本機 API / RTSP 帳密）換成 ${TNVR_n} 佔位符。
/// 佔位符版本寫入 data/go2rtc.json（不含任何密碼），真正的值透過環境變數傳給 go2rtc；
/// go2rtc 讀取設定檔時會代換 ${VAR}。不放在指令列，因為其他程式可以看到行程的指令列。
fn split_secrets(config: &str) -> (String, Vec<(String, String)>) {
    let mut v: serde_json::Value = serde_json::from_str(config).unwrap_or_default();
    let mut env = Vec::new();
    let mut hide = |s: &mut serde_json::Value| {
        if let Some(text) = s.as_str() {
            let name = format!("TNVR_{}", env.len());
            // 代換發生在 JSON 解析之前，所以環境變數的值要先做 JSON 字串跳脫
            let escaped = serde_json::to_string(text).unwrap_or_default();
            env.push((name.clone(), escaped[1..escaped.len() - 1].to_string()));
            *s = serde_json::Value::String(format!("${{{name}}}"));
        }
    };
    for section in ["api", "rtsp"] {
        for key in ["username", "password"] {
            if let Some(s) = v.get_mut(section).and_then(|x| x.get_mut(key)) {
                hide(s);
            }
        }
    }
    if let Some(streams) = v.get_mut("streams").and_then(|s| s.as_object_mut()) {
        for sources in streams.values_mut() {
            if let Some(arr) = sources.as_array_mut() {
                arr.iter_mut().for_each(&mut hide);
            }
        }
    }
    (serde_json::to_string_pretty(&v).unwrap_or_default(), env)
}

/// 計算 go2rtc 設定（JSON）並與目前的比較；回傳 true 代表有變更（需要重啟 go2rtc）
pub fn write_config_if_changed(state: &AppState, cfg: &Config) -> Result<bool> {
    let text = render_config(state, cfg).to_string();
    let mut cur = state.go2rtc_config.lock().unwrap();
    if *cur == text {
        return Ok(false);
    }
    *cur = text;
    Ok(true)
}

fn render_config(state: &AppState, cfg: &Config) -> serde_json::Value {
    // go2rtc 以空白切割指令列，路徑含空白（例如「Tapo NVR」安裝資料夾）時必須再包一層引號
    let bin = state.ffmpeg_bin.to_string_lossy().replace('\\', "/");
    let bin = if bin.contains(' ') { format!("\"{bin}\"") } else { bin };
    let mut streams = serde_json::Map::new();
    for cam in &cfg.cameras {
        let (main, sub) = match cam.stream {
            crate::config::StreamSource::Tapo => (cam.tapo_sources(false), cam.tapo_sources(true)),
            crate::config::StreamSource::Rtsp => (vec![cam.rtsp_url("stream1")], vec![cam.rtsp_url("stream2")]),
        };
        streams.insert(cam.main_stream(), serde_json::json!(main));
        streams.insert(cam.sub_stream(), serde_json::json!(sub));
        // 雙向語音：Tapo 私有協定（需雲端密碼），只在使用者按下「對講」時才連線
        if let Some(talk) = cam.talk_stream() {
            streams.insert(talk, serde_json::json!([cam.tapo_url()]));
        }
    }
    let mut webrtc = serde_json::json!({"listen": format!(":{}", cfg.go2rtc.webrtc_port)});
    if !cfg.go2rtc.webrtc_candidates.is_empty() {
        webrtc["candidates"] = serde_json::json!(cfg.go2rtc.webrtc_candidates);
    }
    serde_json::json!({
        "ffmpeg": {"bin": bin},
        "api": {
            "listen": format!("127.0.0.1:{}", cfg.go2rtc.api_port),
            "username": state.go2rtc_user,
            "password": state.go2rtc_pass,
            // go2rtc 預設對 127.0.0.1 不檢查帳密；本機其他程式也必須驗證
            "local_auth": true,
        },
        "rtsp": {
            "listen": format!("127.0.0.1:{}", cfg.go2rtc.rtsp_port),
            "username": state.go2rtc_user,
            "password": state.go2rtc_pass,
        },
        "webrtc": webrtc,
        "log": {"level": "info"},
        "streams": streams,
    })
}

/// 錄影用的本機 RTSP 轉送網址（含隨機帳密）
pub fn local_rtsp(state: &AppState, stream: &str) -> String {
    format!(
        "rtsp://{}:{}@127.0.0.1:{}/{stream}",
        state.go2rtc_user,
        state.go2rtc_pass,
        state.cfg().go2rtc.rtsp_port
    )
}

/// 啟動並監控 go2rtc：異常結束時自動重啟；設定變更時由 go2rtc_restart 觸發重啟
pub async fn supervise(state: AppState) {
    let binary = state.go2rtc_bin.clone();
    let mut backoff = 1u64;
    loop {
        if state.shutdown.is_cancelled() {
            return;
        }
        let config = state.go2rtc_config.lock().unwrap().clone();
        let (public, env) = split_secrets(&config);
        let path = state.data_dir.join("go2rtc.json");
        if let Err(e) = std::fs::write(&path, public) {
            tracing::error!("無法寫入 {}：{e}", path.display());
        }
        let mut cmd = tokio::process::Command::new(&binary);
        cmd.arg("-config")
            .arg(&path)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                tracing::info!("go2rtc 已啟動 (pid {:?})", child.id());
                if let Some(out) = child.stdout.take() {
                    tokio::spawn(forward_log(BufReader::new(out)));
                }
                if let Some(err) = child.stderr.take() {
                    tokio::spawn(forward_log(BufReader::new(err)));
                }
                let started = std::time::Instant::now();
                tokio::select! {
                    status = child.wait() => {
                        tracing::error!("go2rtc 已結束：{status:?}");
                        if started.elapsed() > Duration::from_secs(60) {
                            backoff = 1;
                        }
                    }
                    _ = state.go2rtc_restart.notified() => {
                        let _ = child.kill().await;
                        backoff = 1;
                        continue;
                    }
                    _ = state.shutdown.cancelled() => {
                        let _ = child.kill().await;
                        return;
                    }
                }
            }
            Err(e) => tracing::error!("無法啟動 go2rtc（{}）：{e}", binary.display()),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
            _ = state.go2rtc_restart.notified() => {}
            _ = state.shutdown.cancelled() => return,
        }
        backoff = (backoff * 2).min(30);
    }
}

async fn forward_log<R: tokio::io::AsyncRead + Unpin>(reader: BufReader<R>) {
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::info!(target: "go2rtc", "{line}");
    }
}

pub async fn snapshot(state: &AppState, stream: &str) -> Result<Bytes> {
    let url = format!("{}/api/frame.jpeg", state.go2rtc_base());
    let resp = state
        .http
        .get(url)
        .basic_auth(&state.go2rtc_user, Some(&state.go2rtc_pass))
        .query(&[("src", stream)])
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context(tr!("go2rtc 無回應", "go2rtc is not responding"))?;
    if !resp.status().is_success() {
        bail!("{}", tr!("擷取畫面失敗：{}", "Snapshot failed: {}", resp.status()));
    }
    Ok(resp.bytes().await?)
}

/// 只代理播放器需要的兩支 JS，避免暴露 go2rtc 的管理 API
pub async fn player_js(State(state): State<AppState>, AxPath(file): AxPath<String>) -> Response {
    if file != "video-rtc.js" && file != "video-stream.js" {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state
        .http
        .get(format!("{}/{file}", state.go2rtc_base()))
        .basic_auth(&state.go2rtc_user, Some(&state.go2rtc_pass))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => (
                [
                    (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
                    (header::CACHE_CONTROL, "public, max-age=3600"),
                ],
                b,
            )
                .into_response(),
            Err(_) => StatusCode::BAD_GATEWAY.into_response(),
        },
        _ => StatusCode::BAD_GATEWAY.into_response(),
    }
}

#[derive(Deserialize)]
pub struct WsQuery {
    src: String,
}

pub async fn ws_proxy(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // 只允許設定檔中的串流，防止透過 src 建立任意來源（例如 exec:）
    let cfg = state.cfg();
    let allowed = cfg
        .cameras
        .iter()
        .any(|c| {
            c.main_stream() == query.src
                || c.sub_stream() == query.src
                || c.talk_stream().as_deref() == Some(query.src.as_str())
        });
    if !allowed {
        return StatusCode::NOT_FOUND.into_response();
    }
    let upstream = format!("ws://127.0.0.1:{}/api/ws?src={}", cfg.go2rtc.api_port, query.src);
    let auth = format!(
        "Basic {}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("{}:{}", state.go2rtc_user, state.go2rtc_pass))
    );
    ws.max_message_size(16 << 20)
        .on_upgrade(move |socket| async move {
            if let Err(e) = pump(socket, &upstream, &auth).await {
                tracing::debug!("串流代理結束：{e}");
            }
        })
}

async fn pump(client: WebSocket, upstream: &str, auth: &str) -> Result<()> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = upstream.into_client_request()?;
    req.headers_mut().insert("Authorization", auth.parse()?);
    let (server, _) = tokio_tungstenite::connect_async(req)
        .await
        .context(tr!("無法連線 go2rtc", "Cannot connect to go2rtc"))?;
    let (mut c_tx, mut c_rx) = client.split();
    let (mut s_tx, mut s_rx) = server.split();

    let up = async {
        while let Some(Ok(msg)) = c_rx.next().await {
            let m = match msg {
                AMsg::Text(t) => TMsg::text(t.as_str().to_owned()),
                AMsg::Binary(b) => TMsg::binary(b),
                AMsg::Ping(b) => TMsg::Ping(b),
                AMsg::Pong(b) => TMsg::Pong(b),
                AMsg::Close(_) => break,
            };
            if s_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = s_tx.close().await;
    };
    let down = async {
        while let Some(Ok(msg)) = s_rx.next().await {
            let m = match msg {
                TMsg::Text(t) => AMsg::Text(t.as_str().to_owned().into()),
                TMsg::Binary(b) => AMsg::Binary(b),
                TMsg::Ping(b) => AMsg::Ping(b),
                TMsg::Pong(b) => AMsg::Pong(b),
                TMsg::Close(_) => break,
                TMsg::Frame(_) => continue,
            };
            if c_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = c_tx.close().await;
    };
    tokio::select! {
        _ = up => {},
        _ = down => {},
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn secrets_are_moved_to_env() {
        let cfg = r#"{"api":{"username":"u1","password":"p1"},"rtsp":{"username":"u1","password":"p1"},
            "streams":{"a_main":["rtsp://cam:p@ss \"x\"@1.2.3.4/stream1"]},"log":{"level":"info"}}"#;
        let (public, env) = super::split_secrets(cfg);
        assert!(!public.contains("p1") && !public.contains("p@ss"));
        assert!(public.contains("${TNVR_0}"));
        // 代換回去後必須仍是合法 JSON，且值與原本相同
        let mut restored = public.clone();
        for (k, v) in &env {
            restored = restored.replace(&format!("${{{k}}}"), v);
        }
        let v: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert_eq!(v["streams"]["a_main"][0], "rtsp://cam:p@ss \"x\"@1.2.3.4/stream1");
        assert_eq!(v["api"]["password"], "p1");
    }
}