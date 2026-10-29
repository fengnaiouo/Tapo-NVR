//! 移動偵測：ONVIF PullPoint 訂閱 → 事件紀錄、截圖、即時推播。

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

use crate::config::CameraConfig;
use crate::onvif::{OnvifClient, OnvifEvent};
use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub ts: DateTime<Utc>,
    pub camera: String,
    pub camera_name: String,
    pub kind: String,
    pub snapshot: Option<String>,
    /// 是否推送通知（總開關與該攝影機的通知設定）
    #[serde(default = "default_true")]
    pub notify: bool,
}

fn default_true() -> bool {
    true
}

type Intervals = HashMap<String, VecDeque<(DateTime<Utc>, Option<DateTime<Utc>>)>>;

#[derive(Debug, Clone, Default, Serialize)]
pub struct CameraStatus {
    pub online: Option<bool>,
    pub active: Vec<String>,
    pub error: Option<String>,
}

pub struct EventHub {
    pub tx: broadcast::Sender<String>,
    recent: Mutex<VecDeque<Event>>,
    status: Mutex<HashMap<String, CameraStatus>>,
    intervals: Mutex<Intervals>,
    store: Arc<crate::store::Store>,
    pub snapshot_dir: PathBuf,
    max_events: AtomicUsize,
}

impl EventHub {
    /// 事件存在資料庫，記憶體保留最新的 max_events 筆供快速查詢
    pub fn load(store: Arc<crate::store::Store>, data_dir: &std::path::Path, max_events: usize) -> Self {
        let snapshot_dir = data_dir.join("snapshots");
        let _ = std::fs::create_dir_all(&snapshot_dir);
        let recent: VecDeque<Event> = store.recent_events(max_events).unwrap_or_default().into();
        let (tx, _) = broadcast::channel(256);
        Self {
            tx,
            recent: Mutex::new(recent),
            status: Mutex::new(HashMap::new()),
            intervals: Mutex::new(HashMap::new()),
            store,
            snapshot_dir,
            max_events: AtomicUsize::new(max_events),
        }
    }

    pub fn set_max_events(&self, n: usize) {
        self.max_events.store(n, Ordering::Relaxed);
    }

    pub fn broadcast(&self, msg: serde_json::Value) {
        let _ = self.tx.send(msg.to_string());
    }

    /// 記錄移動開始 / 結束（事件錄影依此決定保留哪些片段，保留 24 小時）
    pub async fn mark_motion(&self, camera: &str, active: bool) {
        let now = Utc::now();
        let mut map = self.intervals.lock().await;
        let q = map.entry(camera.to_string()).or_default();
        if active {
            if q.back().is_none_or(|(_, end)| end.is_some()) {
                q.push_back((now, None));
            }
        } else if let Some(last) = q.back_mut() {
            if last.1.is_none() {
                last.1 = Some(now);
            }
        }
        let cutoff = now - chrono::Duration::hours(24);
        while q.front().is_some_and(|(_, end)| end.is_some_and(|e| e < cutoff)) {
            q.pop_front();
        }
    }

    pub async fn motion_intervals(&self, camera: &str) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
        let now = Utc::now();
        self.intervals
            .lock()
            .await
            .get(camera)
            .map(|q| q.iter().map(|(s, e)| (*s, e.unwrap_or(now))).collect())
            .unwrap_or_default()
    }

    /// 移除已刪除攝影機的狀態
    pub async fn retain_cameras(&self, ids: &[String]) {
        self.status.lock().await.retain(|k, _| ids.contains(k));
    }

    pub async fn recent(&self, limit: usize, camera: Option<&str>) -> Vec<Event> {
        self.recent
            .lock()
            .await
            .iter()
            .rev()
            .filter(|e| camera.is_none_or(|c| e.camera == c))
            .take(limit)
            .cloned()
            .collect()
    }

    pub async fn status(&self, camera: &str) -> CameraStatus {
        self.status.lock().await.get(camera).cloned().unwrap_or_default()
    }

    async fn update_status(&self, camera: &str, f: impl FnOnce(&mut CameraStatus)) {
        let snapshot = {
            let mut map = self.status.lock().await;
            let s = map.entry(camera.to_string()).or_default();
            f(s);
            s.clone()
        };
        let _ = self
            .tx
            .send(json!({"type": "status", "camera": camera, "status": snapshot}).to_string());
    }

    async fn record(&self, mut ev: Event) {
        match self.store.insert_event(&ev) {
            Ok(id) => ev.id = id,
            Err(e) => tracing::error!("無法寫入事件：{e}"),
        }
        let max = self.max_events.load(Ordering::Relaxed);
        {
            let mut r = self.recent.lock().await;
            r.push_back(ev.clone());
            while r.len() > max {
                r.pop_front();
            }
        }
        // 超過上限的舊事件連同截圖一起刪除
        for f in self.store.prune_events(max).unwrap_or_default() {
            let _ = tokio::fs::remove_file(self.snapshot_dir.join(f)).await;
        }
        let _ = self.tx.send(json!({"type": "event", "event": ev}).to_string());
    }
}

fn classify(ev: &OnvifEvent) -> Option<(&'static str, bool)> {
    let active = match ev.value.to_ascii_lowercase().as_str() {
        "true" | "1" => true,
        "false" | "0" => false,
        _ => return None,
    };
    let t = format!("{} {}", ev.topic, ev.name).to_ascii_lowercase();
    let kind = if t.contains("people") || t.contains("person") || t.contains("human") {
        "person"
    } else if t.contains("tamper") {
        "tamper"
    } else if t.contains("motion") {
        "motion"
    } else {
        return None;
    };
    Some((kind, active))
}

/// 每台攝影機一個背景工作：訂閱 → 拉取 → 失敗時退避重連；設定變更時由 cancel 結束
pub async fn watch_camera(
    state: AppState,
    cam: CameraConfig,
    client: Arc<OnvifClient>,
    cancel: CancellationToken,
) {
    let hub = &state.events;
    let mut last_notify: HashMap<&'static str, Instant> = HashMap::new();
    let mut backoff = 5u64;

    loop {
        let subscribed = tokio::select! {
            _ = cancel.cancelled() => break,
            r = client.subscribe() => r,
        };
        match subscribed {
            Ok(sub) => {
                tracing::info!("[{}] 已訂閱 ONVIF 事件", cam.id);
                backoff = 5;
                hub.update_status(&cam.id, |s| {
                    s.online = Some(true);
                    s.error = None;
                })
                .await;
                let mut renewed = Instant::now();
                loop {
                    if renewed.elapsed() > Duration::from_secs(240) {
                        if let Err(e) = client.renew(&sub).await {
                            tracing::warn!("[{}] 續訂失敗：{e}", cam.id);
                            break;
                        }
                        renewed = Instant::now();
                    }
                    let pulled = tokio::select! {
                        _ = cancel.cancelled() => break,
                        r = client.pull(&sub) => r,
                    };
                    let msgs = match pulled {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!("[{}] 拉取事件失敗：{e}", cam.id);
                            break;
                        }
                    };
                    let cooldown = Duration::from_secs(state.cfg().notify.cooldown_secs);
                    for m in msgs {
                        let Some((kind, active)) = classify(&m) else { continue };
                        let was_active = hub.status(&cam.id).await.active.iter().any(|k| k == kind);
                        if active == was_active {
                            continue;
                        }
                        hub.update_status(&cam.id, |s| {
                            if active {
                                s.active.push(kind.to_string());
                            } else {
                                s.active.retain(|k| k != kind);
                            }
                        })
                        .await;
                        let any_active = !hub.status(&cam.id).await.active.is_empty();
                        hub.mark_motion(&cam.id, any_active).await;
                        if !active {
                            continue;
                        }
                        if last_notify.get(kind).is_some_and(|t| t.elapsed() < cooldown) {
                            continue;
                        }
                        last_notify.insert(kind, Instant::now());
                        let st = state.clone();
                        let c = cam.clone();
                        tokio::spawn(async move { on_trigger(st, c, kind).await });
                    }
                }
                // 連線中斷時結束進行中的移動區間；重新訂閱後攝影機會再回報目前狀態
                hub.mark_motion(&cam.id, false).await;
                hub.update_status(&cam.id, |s| s.active.clear()).await;
                // 釋放攝影機端的訂閱名額（Tapo 可同時訂閱的數量有限）
                client.unsubscribe(&sub).await;
            }
            Err(e) => {
                tracing::warn!("[{}] 無法訂閱事件：{e}", cam.id);
                hub.update_status(&cam.id, |s| {
                    s.online = Some(false);
                    s.active.clear();
                    s.error = Some(e.to_string());
                })
                .await;
                hub.mark_motion(&cam.id, false).await;
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
        }
        backoff = (backoff * 2).min(60);
    }
    tracing::info!("[{}] 停止移動偵測", cam.id);
    hub.mark_motion(&cam.id, false).await;
    hub.update_status(&cam.id, |s| {
        s.online = None;
        s.active.clear();
        s.error = None;
    })
    .await;
}

async fn on_trigger(state: AppState, cam: CameraConfig, kind: &'static str) {
    let ts = Utc::now();
    let cfg = state.cfg();
    let mut snapshot = None;
    let mut jpeg = None;
    if cfg.notify.snapshot {
        match crate::go2rtc::snapshot(&state, &cam.main_stream()).await {
            Ok(jpg) => {
                let name = format!("{}_{}.jpg", cam.id, ts.format("%Y%m%d_%H%M%S_%3f"));
                if tokio::fs::write(state.events.snapshot_dir.join(&name), &jpg).await.is_ok() {
                    snapshot = Some(name);
                }
                jpeg = Some(jpg);
            }
            Err(e) => tracing::warn!("[{}] 截圖失敗：{e}", cam.id),
        }
    }
    let notify = cfg.notify.enabled && cam.notify;
    tracing::info!("[{}] 偵測到 {kind}", cam.id);
    let event = Event {
        id: 0,
        ts,
        camera: cam.id.clone(),
        camera_name: cam.name.clone(),
        kind: kind.into(),
        snapshot,
        notify,
    };
    state.events.record(event.clone()).await;
    if notify {
        crate::notify::push(&state, &event, jpeg).await;
    }
}

pub async fn events_ws(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| client_loop(state, socket))
}

async fn client_loop(state: AppState, mut socket: WebSocket) {
    let mut rx = state.events.tx.subscribe();
    let mut ping = tokio::time::interval(Duration::from_secs(25));
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Default::default())).await.is_err() { break; }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}
