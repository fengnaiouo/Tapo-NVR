//! 攝影機連線監控：每 30 秒以 TCP 連線檢查每台攝影機，離線 / 恢復時記錄事件並通知。
//! 不依賴 ONVIF 事件訂閱，所以沒開移動偵測的攝影機也能偵測離線。
//!
//! IP 變動自動找回：連線正常時記下攝影機的 MAC；離線時掃描區網，
//! 找到同一個 MAC 出現在新 IP 就自動更新設定（例如路由器重開、改成 AP 模式後 IP 變了）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::config::{normalize_mac, CameraConfig, StreamSource};
use crate::events::Event;
use crate::AppState;

const INTERVAL: Duration = Duration::from_secs(30);
/// 連續失敗幾次才判定離線（避免網路短暫抖動就通知）
const FAIL_THRESHOLD: u32 = 2;
/// 同一台攝影機兩次自動找回之間的最短間隔
const RECOVER_INTERVAL: Duration = Duration::from_secs(180);

#[derive(Default)]
struct Track {
    failures: u32,
    /// 已對外宣告的狀態：None = 尚未確定
    announced: Option<bool>,
    last_recover: Option<Instant>,
    mac_tried: bool,
}

fn probe_port(cam: &CameraConfig) -> u16 {
    match cam.stream {
        StreamSource::Rtsp => cam.rtsp_port,
        // Tapo 私有串流走 HTTPS 443
        StreamSource::Tapo => 443,
    }
}

async fn probe(host: &str, port: u16) -> Result<(), String> {
    let addr = format!("{host}:{port}");
    match tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(tr!("連線逾時", "Connection timed out")),
    }
}

/// 從系統 ARP 表查 IP 對應的 MAC（剛連線過，表中一定有）
async fn arp_mac(host: &str) -> Option<String> {
    #[cfg(windows)]
    let output = {
        let mut cmd = tokio::process::Command::new("arp");
        cmd.args(["-a", host]);
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.output().await.ok()?
    };
    #[cfg(not(windows))]
    let output = tokio::process::Command::new("ip").args(["neigh", "show", host]).output().await.ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find(|tok| tok.len() == 17 && tok.matches(['-', ':']).count() == 5)
        .map(normalize_mac)
        .filter(|m| m.len() == 12 && m != "FFFFFFFFFFFF" && m != "000000000000")
}

pub async fn monitor(state: AppState) {
    let mut tracks: HashMap<String, Track> = HashMap::new();
    loop {
        let cfg = state.cfg();
        tracks.retain(|id, _| cfg.cameras.iter().any(|c| &c.id == id));
        let results = futures_util::future::join_all(
            cfg.cameras.iter().map(|c| async move { (c.clone(), probe(&c.host, probe_port(c)).await) }),
        )
        .await;
        for (cam, result) in results {
            let t = tracks
                .entry(cam.id.clone())
                .or_insert_with(|| Track { announced: state.events.last_reach_state(&cam.id), ..Default::default() });
            let ok = result.is_ok();
            if ok {
                t.failures = 0;
                // 第一次連線成功時記下 MAC（之後 IP 變了才找得回來）
                if cam.mac.is_empty() && !t.mac_tried {
                    t.mac_tried = true;
                    if let Some(mac) = arp_mac(&cam.host).await {
                        let id = cam.id.clone();
                        let m = mac.clone();
                        let r = state
                            .update_config(move |c| {
                                if let Some(x) = c.cameras.iter_mut().find(|x| x.id == id) {
                                    x.mac = m;
                                }
                                Ok(())
                            })
                            .await;
                        if r.is_ok() {
                            tracing::info!("[{}] 已記錄 MAC {mac}", cam.id);
                        }
                    }
                }
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
            if let Some(s) = now_state {
                state.events.set_reachable(&cam.id, s, result.clone().err()).await;
            }

            // 離線：嘗試用 MAC 在區網中找回攝影機
            if now_state == Some(false)
                && !cam.mac.is_empty()
                && t.last_recover.is_none_or(|at| at.elapsed() >= RECOVER_INTERVAL)
            {
                t.last_recover = Some(Instant::now());
                if recover(&state, &cam).await {
                    // 設定已更新，下一輪會用新 IP 檢查
                    t.failures = 0;
                    continue;
                }
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
            let notify = n.offline_alert && n.enabled && cam.wants_notify(kind) && n.in_schedule(chrono::Local::now());
            let ev = state.events.record_event(new_event(&cam, kind, notify)).await;
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

fn new_event(cam: &CameraConfig, kind: &str, notify: bool) -> Event {
    Event {
        id: 0,
        ts: Utc::now(),
        camera: cam.id.clone(),
        camera_name: cam.name.clone(),
        kind: kind.into(),
        snapshot: None,
        notify,
    }
}

/// 掃描區網，找到同 MAC 的裝置出現在新 IP 時更新設定；回傳是否已更新
async fn recover(state: &AppState, cam: &CameraConfig) -> bool {
    let found = crate::discovery::probe(Duration::from_secs(3)).await;
    let Some(hit) = found.into_iter().find(|f| normalize_mac(&f.mac) == cam.mac && f.host != cam.host) else {
        tracing::info!("[{}] 區網中找不到 MAC {} 的攝影機", cam.id, cam.mac);
        return false;
    };
    let new_host = hit.host.clone();
    if probe(&new_host, probe_port(cam)).await.is_err() {
        tracing::warn!("[{}] 在 {new_host} 找到同 MAC 的裝置，但連接埠 {} 無法連線", cam.id, probe_port(cam));
        return false;
    }
    let id = cam.id.clone();
    let h = new_host.clone();
    let result = state
        .update_config(move |c| {
            if c.cameras.iter().any(|x| x.id != id && x.host == h) {
                anyhow::bail!("{}", tr!("{h} 已被其他攝影機使用", "{h} is already used by another camera"));
            }
            if let Some(x) = c.cameras.iter_mut().find(|x| x.id == id) {
                x.host = h;
            }
            Ok(())
        })
        .await;
    match result {
        Ok(()) => {
            tracing::info!("[{}] 攝影機 IP 已從 {} 自動更新為 {new_host}", cam.id, cam.host);
            state.events.record_event(new_event(cam, "ip_changed", false)).await;
            true
        }
        Err(e) => {
            tracing::warn!("[{}] 無法更新 IP：{e:#}", cam.id);
            false
        }
    }
}
