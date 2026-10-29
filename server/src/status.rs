//! 系統狀態總覽：一次取得伺服器、儲存空間、每台攝影機（連線、移動偵測、錄影、事件）的狀態，
//! 並自動整理出需要注意的問題。

use std::time::Duration;

use axum::extract::State;
use axum::Json;
use chrono::{Local, Utc};
use serde_json::{json, Value};

use crate::config::RecordMode;
use crate::AppState;

const LOW_DISK: u64 = 10 * 1024 * 1024 * 1024;

async fn go2rtc_alive(state: &AppState) -> bool {
    state
        .http
        .get(format!("{}/api", state.go2rtc_base()))
        .basic_auth(&state.go2rtc_user, Some(&state.go2rtc_pass))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

pub async fn status(State(state): State<AppState>) -> Json<Value> {
    let cfg = state.cfg();
    let now = Utc::now();
    let local_now = Local::now();
    let mut warnings: Vec<Value> = Vec::new();
    let warn = |w: &mut Vec<Value>, level: &str, text: String, link: &str| {
        w.push(json!({"level": level, "text": text, "link": link}));
    };

    // 儲存空間
    let root = cfg.recording.root();
    let ids: Vec<String> = cfg.cameras.iter().map(|c| c.id.clone()).collect();
    let (used_total, per_cam, free) = match &root {
        Some(r) => {
            let r2 = r.clone();
            let ids2 = ids.clone();
            let (total, per) = tokio::task::spawn_blocking(move || {
                let per: Vec<u64> = ids2.iter().map(|id| crate::recorder::dir_size(&r2.join(id))).collect();
                (crate::recorder::dir_size(&r2), per)
            })
            .await
            .unwrap_or_default();
            (Some(total), per, crate::free_space(r))
        }
        None => (None, vec![0; ids.len()], None),
    };
    match (&root, free) {
        (None, _) => warn(
            &mut warnings,
            "warn",
            tr!("尚未設定錄影資料夾，目前不會錄影", "No recording folder set, nothing is being recorded"),
            "/settings.html#recording",
        ),
        (Some(r), _) if !r.exists() => warn(
            &mut warnings,
            "error",
            tr!("錄影資料夾不存在：{}", "Recording folder does not exist: {}", r.display()),
            "/settings.html#recording",
        ),
        (Some(_), Some(f)) if f < LOW_DISK => warn(
            &mut warnings,
            "warn",
            tr!("錄影磁碟剩餘空間不足 {:.1} GB，將自動刪除最舊的錄影", "Recording disk has less than {:.1} GB free; oldest recordings will be deleted", f as f64 / 1e9),
            "/settings.html#recording",
        ),
        _ => {}
    }

    // 攝影機
    let mut cameras = Vec::new();
    for (i, cam) in cfg.cameras.iter().enumerate() {
        let st = state.events.status(&cam.id).await;
        let rec = state.recorders.status(&cam.id);
        let mode_now = cam.record_mode_at(local_now);
        let events = state.events.events_between(&cam.id, now - chrono::Duration::hours(24), now);
        let last = events.last().map(|e| json!({"ts": e.ts, "kind": e.kind}));
        let detections = events.iter().filter(|e| crate::events::DETECTION_KINDS.contains(&e.kind.as_str())).count();

        if st.reachable == Some(false) {
            warn(
                &mut warnings,
                "error",
                tr!("「{}」離線（{}）", "\"{}\" is offline ({})", cam.name, st.reach_error.clone().unwrap_or_default()),
                "/settings.html#cameras",
            );
        } else if cam.motion && st.online == Some(false) {
            warn(
                &mut warnings,
                "warn",
                tr!("「{}」的移動偵測連線異常：{}", "Motion detection for \"{}\" is failing: {}", cam.name, st.error.clone().unwrap_or_default()),
                "/settings.html#system",
            );
        }
        if mode_now != RecordMode::Off && root.is_some() && rec.as_ref().is_some_and(|r| r.state == "error") {
            warn(
                &mut warnings,
                "error",
                tr!("「{}」錄影異常：{}", "Recording error on \"{}\": {}", cam.name, rec.as_ref().and_then(|r| r.error.clone()).unwrap_or_default()),
                "/settings.html#system",
            );
        }
        cameras.push(json!({
            "id": cam.id,
            "name": cam.name,
            "host": cam.host,
            "mac": cam.mac,
            "model": cam.model,
            "reachable": st.reachable,
            "reach_error": st.reach_error,
            "motion_enabled": cam.motion,
            "motion_online": st.online,
            "motion_error": st.error,
            "active": st.active,
            "record_mode": cam.record,
            "record_mode_now": mode_now,
            "has_schedule": !cam.schedule.is_empty(),
            "recording": rec,
            "used_bytes": per_cam.get(i).copied().unwrap_or(0),
            "events_24h": events.len(),
            "detections_24h": detections,
            "last_event": last,
        }));
    }

    let go2rtc = go2rtc_alive(&state).await;
    if !go2rtc {
        warn(
            &mut warnings,
            "error",
            tr!("串流服務 go2rtc 沒有回應，即時影像與錄影可能中斷", "The go2rtc streaming service is not responding; live view and recording may be affected"),
            "/settings.html#system",
        );
    }
    if !cfg.notify.enabled {
        warn(&mut warnings, "info", tr!("目前為撤防狀態，不會推送通知", "Disarmed: notifications are paused"), "/");
    }
    if state.restart_required() {
        warn(&mut warnings, "info", tr!("部分設定需要重新啟動伺服器才會生效", "Some settings take effect after restarting the server"), "/settings.html#system");
    }

    Json(json!({
        "server": {
            "version": crate::about::VERSION,
            "desktop": state.desktop_embedded,
            "started_at": state.started_at,
            "uptime_secs": (now - state.started_at).num_seconds().max(0),
            "go2rtc": go2rtc,
            "armed": cfg.notify.enabled,
            "in_schedule": cfg.notify.in_schedule(local_now),
            "data_path": state.store.path.display().to_string(),
        },
        "storage": {
            "path": cfg.recording.path,
            "enabled": root.is_some(),
            "used_bytes": used_total,
            "free_bytes": free,
            "max_gb": cfg.recording.max_gb,
            "max_days": cfg.recording.max_days,
        },
        "cameras": cameras,
        "warnings": warnings,
    }))
}
