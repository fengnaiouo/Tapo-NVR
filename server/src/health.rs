//! 攝影機連線監控：每 30 秒以 TCP 連線檢查每台攝影機，離線 / 恢復時記錄事件並通知。
//! 不依賴 ONVIF 事件訂閱，所以沒開移動偵測的攝影機也能偵測離線。

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;

use crate::config::{CameraConfig, StreamSource};
use crate::events::Event;
use crate::AppState;

const INTERVAL: Duration = Duration::from_secs(30);
/// 連續失敗幾次才判定離線（避免網路短暫抖動就通知）
const FAIL_THRESHOLD: u32 = 2;

#[derive(Default)]
struct Track {
    failures: u32,
    /// 已對外宣告的狀態：None = 尚未確定
    announced: Option<bool>,
}

async fn probe(cam: &CameraConfig) -> Result<(), String> {
    let port = match cam.stream {
        StreamSource::Rtsp => cam.rtsp_port,
        // Tapo 私有串流走 HTTPS 443
        StreamSource::Tapo => 443,
    };
    let addr = format!("{}:{port}", cam.host);
    match tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(tr!("連線逾時", "Connection timed out")),
    }
}

pub async fn monitor(state: AppState) {
    let mut tracks: HashMap<String, Track> = HashMap::new();
    loop {
        let cfg = state.cfg();
        tracks.retain(|id, _| cfg.cameras.iter().any(|c| &c.id == id));
        let results = futures_util::future::join_all(cfg.cameras.iter().map(|c| async move { (c.clone(), probe(c).await) })).await;
        for (cam, result) in results {
            let t = tracks
                .entry(cam.id.clone())
                .or_insert_with(|| Track { failures: 0, announced: state.events.last_reach_state(&cam.id) });
            let ok = result.is_ok();
            if ok {
                t.failures = 0;
            } else {
                t.failures += 1;
            }
            let now_state = if ok {
                Some(true)
            } else if t.failures >= FAIL_THRESHOLD {
                Some(false)
            } else {
                t.announced // 還沒達到門檻，維持原狀
            };
            if now_state.is_some() {
                state.events.set_reachable(&cam.id, now_state.unwrap_or(true), result.clone().err()).await;
            }
            if now_state == t.announced || now_state.is_none() {
                continue;
            }
            let previous = t.announced;
            t.announced = now_state;
            // 啟動後第一次確認為「在線」不算事件；離線則一律通知（攝影機可能一開始就斷線）
            let kind = match (previous, now_state) {
                (_, Some(false)) => "offline",
                (Some(false), Some(true)) => "online",
                _ => continue,
            };
            if kind == "offline" {
                tracing::warn!("[{}] 攝影機離線：{}", cam.id, result.err().unwrap_or_default());
            } else {
                tracing::info!("[{}] 攝影機恢復連線", cam.id);
            }
            let n = &cfg.notify;
            let notify = n.offline_alert && n.enabled && cam.notify && n.in_schedule(chrono::Local::now());
            let ev = state
                .events
                .record_event(Event {
                    id: 0,
                    ts: Utc::now(),
                    camera: cam.id.clone(),
                    camera_name: cam.name.clone(),
                    kind: kind.into(),
                    snapshot: None,
                    notify,
                })
                .await;
            if notify {
                crate::notify::push(&state, &ev, None).await;
            }
        }
        tokio::select! {
            _ = state.shutdown.cancelled() => return,
            _ = tokio::time::sleep(INTERVAL) => {}
        }
    }
}
