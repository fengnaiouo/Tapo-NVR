//! 錄影：每台攝影機一個 ffmpeg，從 go2rtc 的 RTSP 轉送取主串流，切成分段 MP4。
//!
//! 目錄結構：{錄影資料夾}/{攝影機代號}/{YYYY-MM-DD}/{HH-MM-SS}.mp4（本地時間）
//! 「事件錄影」模式其實也持續錄製，再由清理工作刪掉沒有移動的片段，因此自然具備事件前的畫面。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::{CameraConfig, RecordMode};
use crate::AppState;

#[derive(Clone, PartialEq)]
struct RecKey {
    cam: CameraConfig,
    root: PathBuf,
    segment_seconds: u64,
    rtsp_port: u16,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RecStatus {
    /// recording / error / starting
    pub state: String,
    pub error: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

#[derive(Default)]
pub struct Recorders {
    tasks: Mutex<HashMap<String, (RecKey, CancellationToken, tokio::task::JoinHandle<()>)>>,
    status: std::sync::Mutex<HashMap<String, RecStatus>>,
}

impl Recorders {
    pub fn status(&self, id: &str) -> Option<RecStatus> {
        self.status.lock().unwrap().get(id).cloned()
    }
    fn set(&self, id: &str, s: RecStatus) {
        self.status.lock().unwrap().insert(id.to_string(), s);
    }
    fn clear(&self, id: &str) {
        self.status.lock().unwrap().remove(id);
    }
}

/// 依目前設定啟動 / 停止 / 重啟各攝影機的錄影
pub async fn sync(state: &AppState) {
    let cfg = state.cfg();
    let root = cfg.recording.root();
    let mut wanted: HashMap<String, RecKey> = HashMap::new();
    if let Some(root) = root {
        // 依錄影排程決定目前要不要錄；連續與事件模式都是持續錄影（事件模式事後修剪），切換時不需重啟
        let now = Local::now();
        for cam in cfg.cameras.iter().filter(|c| c.record_mode_at(now) != RecordMode::Off) {
            wanted.insert(
                cam.id.clone(),
                RecKey {
                    cam: cam.clone(),
                    root: root.clone(),
                    segment_seconds: cfg.recording.segment_seconds,
                    rtsp_port: cfg.go2rtc.rtsp_port,
                },
            );
        }
    }
    let mut tasks = state.recorders.tasks.lock().await;
    let stale: Vec<String> = tasks
        .iter()
        .filter(|(id, (key, _, _))| wanted.get(*id) != Some(key))
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        if let Some((_, token, handle)) = tasks.remove(&id) {
            token.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }
    }
    for (id, key) in wanted {
        if tasks.contains_key(&id) {
            continue;
        }
        let token = CancellationToken::new();
        let handle = tokio::spawn(run(state.clone(), key.clone(), token.clone()));
        tasks.insert(id, (key, token, handle));
    }
}

pub async fn stop_all(state: &AppState) {
    let mut tasks = state.recorders.tasks.lock().await;
    for (_, (_, token, handle)) in tasks.drain() {
        token.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }
}

async fn run(state: AppState, key: RecKey, cancel: CancellationToken) {
    let id = key.cam.id.clone();
    let mut backoff = 2u64;
    tracing::info!("[{id}] 開始錄影（{:?}）→ {}", key.cam.record, key.root.display());
    loop {
        if cancel.is_cancelled() || state.shutdown.is_cancelled() {
            break;
        }
        let now = Local::now();
        let day_dir = key.root.join(&id).join(now.format("%Y-%m-%d").to_string());
        if let Err(e) = std::fs::create_dir_all(&day_dir) {
            state.recorders.set(&id, RecStatus {
                state: "error".into(),
                error: Some(tr!("無法建立錄影資料夾：{e}", "Cannot create recording folder: {e}")),
                since: Some(Utc::now()),
            });
            if wait_or_cancel(&cancel, 30).await {
                break;
            }
            continue;
        }
        // 跨日時結束 ffmpeg，下一輪改寫到新的日期資料夾
        let tomorrow = now.date_naive().succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap();
        let until_midnight = Local
            .from_local_datetime(&tomorrow)
            .earliest()
            .map(|t| (t - now).num_seconds().max(1) + 1)
            .unwrap_or(3600);

        let _ = key.rtsp_port;
        let input = crate::go2rtc::local_rtsp(&state, &key.cam.main_stream());
        // 錄製中為 fragmented MP4（當機也不會損毀），寫完後由 finalize 轉成可拖曳的一般 MP4
        let output = day_dir.join("%H-%M-%S.rec.mp4");
        let mut cmd = tokio::process::Command::new(&state.ffmpeg_bin);
        cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"])
            .args(["-rtsp_transport", "tcp", "-timeout", "10000000", "-i", &input])
            .args(["-map", "0:v:0", "-map", "0:a:0?", "-c:v", "copy", "-c:a", "aac", "-b:a", "64k"])
            .args(["-t", &until_midnight.to_string()])
            .args(["-f", "segment", "-segment_time", &key.segment_seconds.to_string()])
            .args(["-segment_atclocktime", "1", "-reset_timestamps", "1", "-strftime", "1"])
            .args(["-segment_format", "mp4"])
            .args(["-segment_format_options", "movflags=+frag_keyframe+empty_moov+default_base_moof"])
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                state.recorders.set(&id, RecStatus {
                    state: "error".into(),
                    error: Some(tr!("找不到或無法執行 ffmpeg（{}）：{e}", "Cannot run ffmpeg ({}): {e}", state.ffmpeg_bin.display())),
                    since: Some(Utc::now()),
                });
                if wait_or_cancel(&cancel, 60).await {
                    break;
                }
                continue;
            }
        };
        state.recorders.set(&id, RecStatus { state: "recording".into(), error: None, since: Some(Utc::now()) });

        let last_err = Arc::new(std::sync::Mutex::new(String::new()));
        if let Some(err) = child.stderr.take() {
            let last = last_err.clone();
            let id2 = id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                let mut logged = 0;
                while let Ok(Some(line)) = lines.next_line().await {
                    if logged < 5 {
                        tracing::warn!(target: "ffmpeg", "[{id2}] {line}");
                        logged += 1;
                    }
                    *last.lock().unwrap() = line;
                }
            });
        }

        let started = std::time::Instant::now();
        tokio::select! {
            status = child.wait() => {
                let ran = started.elapsed();
                let ok = status.map(|s| s.success()).unwrap_or(false);
                if ok && ran.as_secs() + 5 >= until_midnight as u64 {
                    backoff = 2;
                    continue; // 正常跨日
                }
                if ran > Duration::from_secs(60) {
                    backoff = 2;
                }
                let msg = last_err.lock().unwrap().clone();
                tracing::warn!("[{id}] 錄影中斷：{msg}");
                state.recorders.set(&id, RecStatus {
                    state: "error".into(),
                    error: Some(if msg.is_empty() { tr!("ffmpeg 已結束，重新連線中", "ffmpeg stopped, reconnecting") } else { msg }),
                    since: Some(Utc::now()),
                });
                if wait_or_cancel(&cancel, backoff).await {
                    break;
                }
                backoff = (backoff * 2).min(60);
            }
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                break;
            }
            _ = state.shutdown.cancelled() => {
                let _ = child.kill().await;
                break;
            }
        }
    }
    state.recorders.clear(&id);
    tracing::info!("[{id}] 停止錄影");
}

/// 回傳 true 代表已取消
async fn wait_or_cancel(cancel: &CancellationToken, secs: u64) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(Duration::from_secs(secs)) => false,
    }
}

// ---------------------------------------------------------------------------
// 片段列表（回放與清理共用）

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub date: String,
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub size: u64,
}

pub fn valid_date(s: &str) -> bool {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() && s.len() == 10
}

/// HH-MM-SS.mp4（已完成）或 HH-MM-SS.rec.mp4（錄製中 / 尚未轉檔）
pub fn valid_segment_name(s: &str) -> bool {
    (s.len() == 12 && s.ends_with(".mp4") || s.len() == 16 && s.ends_with(".rec.mp4"))
        && s.is_char_boundary(8)
        && NaiveDateTime::parse_from_str(&format!("2000-01-01 {}", &s[..8]), "%Y-%m-%d %H-%M-%S").is_ok()
}

pub fn list_days(root: &Path, cam: &str) -> Vec<String> {
    let mut days: Vec<String> = std::fs::read_dir(root.join(cam))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|d| valid_date(d))
        .collect();
    days.sort();
    days
}

pub fn list_day(root: &Path, cam: &str, date: &str) -> Vec<Segment> {
    let dir = root.join(cam).join(date);
    let mut out: Vec<Segment> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !valid_segment_name(&name) {
                return None;
            }
            let naive = NaiveDateTime::parse_from_str(&format!("{date} {}", &name[..8]), "%Y-%m-%d %H-%M-%S").ok()?;
            let start = Local.from_local_datetime(&naive).earliest()?;
            let meta = e.metadata().ok()?;
            let end: DateTime<Local> = meta.modified().ok().map(DateTime::from).unwrap_or(start);
            Some(Segment {
                date: date.to_string(),
                name,
                path: e.path(),
                start,
                end: end.max(start),
                size: meta.len(),
            })
        })
        .collect();
    out.sort_by_key(|s| s.start);
    out
}

fn all_segments(root: &Path) -> Vec<Segment> {
    let cams: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let mut out = Vec::new();
    for cam in cams {
        for day in list_days(root, &cam) {
            out.extend(list_day(root, &cam, &day));
        }
    }
    out
}

pub fn dir_size(root: &Path) -> u64 {
    all_segments(root).iter().map(|s| s.size).sum()
}

// ---------------------------------------------------------------------------
// 清理：事件模式修剪、保留天數、容量上限、磁碟剩餘空間

const MIN_FREE: u64 = 2 * 1024 * 1024 * 1024;
const TARGET_FREE: u64 = 3 * 1024 * 1024 * 1024;

pub async fn retention_loop(state: AppState) {
    let mut tick: u64 = 0;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(15)) => {}
            _ = state.shutdown.cancelled() => return,
        }
        tick += 1;
        // 每分鐘依錄影排程啟動 / 停止錄影
        if tick % 4 == 0 {
            sync(&state).await;
        }
        let Some(root) = state.cfg().recording.root() else { continue };
        let ffmpeg = state.ffmpeg_bin.clone();
        let r = root.clone();
        let _ = tokio::task::spawn_blocking(move || finalize(&r, &ffmpeg)).await;
        if tick % 4 == 0 {
            let job = prepare_job(&state, root).await;
            let _ = tokio::task::spawn_blocking(move || job.run()).await;
        }
    }
}

/// 把已寫完（20 秒未變動）的 .rec.mp4 重新封裝成一般 MP4（moov 在前，可拖曳），不重新編碼
fn finalize(root: &Path, ffmpeg: &Path) {
    let now = std::time::SystemTime::now();
    let today = Local::now();
    let days = [today.format("%Y-%m-%d").to_string(), (today - chrono::Duration::days(1)).format("%Y-%m-%d").to_string()];
    for cam in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for day in &days {
            for e in std::fs::read_dir(cam.path().join(day)).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !(name.len() == 16 && name.ends_with(".rec.mp4")) {
                    continue;
                }
                let settled = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age > Duration::from_secs(20));
                if !settled {
                    continue;
                }
                let src = e.path();
                let tmp = src.with_file_name(format!("{}.tmp.mp4", &name[..8]));
                let dst = src.with_file_name(format!("{}.mp4", &name[..8]));
                let mut cmd = std::process::Command::new(ffmpeg);
                cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i"])
                    .arg(&src)
                    .args(["-c", "copy", "-movflags", "+faststart"])
                    .arg(&tmp)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000);
                }
                match cmd.status() {
                    Ok(s) if s.success() => {
                        // 保留原始時間戳記，讓「結束時間」維持正確
                        if let Ok(mtime) = e.metadata().and_then(|m| m.modified()) {
                            if let Ok(f) = std::fs::File::options().write(true).open(&tmp) {
                                let _ = f.set_modified(mtime);
                            }
                        }
                        if std::fs::rename(&tmp, &dst).is_ok() {
                            let _ = std::fs::remove_file(&src);
                        }
                    }
                    Ok(_) => {
                        // 檔案損毀或太短（例如只有幾秒）：保留原檔，改名避免一直重試
                        let _ = std::fs::remove_file(&tmp);
                        let _ = std::fs::rename(&src, &dst);
                    }
                    Err(_) => return, // 沒有 ffmpeg
                }
            }
        }
    }
}

struct RetentionJob {
    root: PathBuf,
    max_days: u32,
    max_bytes: u64,
    pre: i64,
    post: i64,
    /// 可能使用事件模式的攝影機 → (設定, 需保留的時間區間)
    event_cams: HashMap<String, (CameraConfig, Vec<(DateTime<Utc>, DateTime<Utc>)>)>,
}

async fn prepare_job(state: &AppState, root: PathBuf) -> RetentionJob {
    let cfg = state.cfg();
    let mut event_cams = HashMap::new();
    // 清理只處理最近 2 小時內的片段，往前多取一些涵蓋事件前後保留秒數
    let to = Utc::now();
    let from = to - chrono::Duration::hours(3);
    let uses_events =
        |c: &CameraConfig| c.record == RecordMode::Events || c.schedule.iter().any(|r| r.mode == RecordMode::Events);
    for cam in cfg.cameras.iter().filter(|c| uses_events(c)) {
        event_cams.insert(cam.id.clone(), (cam.clone(), state.events.keep_ranges(&cam.id, from, to)));
    }
    RetentionJob {
        root,
        max_days: cfg.recording.max_days,
        max_bytes: cfg.recording.max_gb.saturating_mul(1_000_000_000),
        pre: cfg.recording.pre_seconds as i64,
        post: cfg.recording.post_seconds as i64,
        event_cams,
    }
}

impl RetentionJob {
    fn run(self) {
        let now = Local::now();
        let mut removed = 0usize;

        // 1. 事件模式：刪除最近 2 小時內、已寫完且與任何移動區間都不重疊的片段
        let settle = chrono::Duration::seconds(self.post + 90);
        for (cam, (cam_cfg, ranges)) in &self.event_cams {
            for day in list_days(&self.root, cam).iter().rev().take(2) {
                for seg in list_day(&self.root, cam, day) {
                    let age = now - seg.end;
                    if age < settle || age > chrono::Duration::hours(2) {
                        continue;
                    }
                    // 錄影排程：只有錄製當時為「事件模式」的片段才修剪
                    if cam_cfg.record_mode_at(seg.start) != RecordMode::Events {
                        continue;
                    }
                    let (s, e) = (seg.start.with_timezone(&Utc), seg.end.with_timezone(&Utc));
                    let keep = ranges.iter().any(|(a, b)| {
                        s <= *b + chrono::Duration::seconds(self.post)
                            && e >= *a - chrono::Duration::seconds(self.pre)
                    });
                    if !keep && std::fs::remove_file(&seg.path).is_ok() {
                        removed += 1;
                    }
                }
            }
        }

        // 2. 保留天數
        if self.max_days > 0 {
            let cutoff = (now - chrono::Duration::days(self.max_days as i64)).format("%Y-%m-%d").to_string();
            for cam in std::fs::read_dir(&self.root).into_iter().flatten().flatten() {
                for day in std::fs::read_dir(cam.path()).into_iter().flatten().flatten() {
                    let name = day.file_name().to_string_lossy().to_string();
                    if valid_date(&name) && name < cutoff && std::fs::remove_dir_all(day.path()).is_ok() {
                        tracing::info!("刪除超過保留天數的錄影：{}", day.path().display());
                    }
                }
            }
        }

        // 3. 容量上限與磁碟剩餘空間：從最舊的開始刪（不動 2 分鐘內仍在寫入的檔案）
        let mut segs = all_segments(&self.root);
        segs.sort_by_key(|s| s.start);
        let mut total: u64 = segs.iter().map(|s| s.size).sum();
        let busy = now - chrono::Duration::minutes(2);
        let mut free = crate::free_space(&self.root);
        // 剩餘空間低於 2 GB 時開始刪，直到回到 3 GB 以上
        let mut cleaning_disk = free.is_some_and(|f| f < MIN_FREE);
        if cleaning_disk {
            tracing::warn!("錄影磁碟剩餘空間不足 2 GB，刪除最舊的錄影");
        }
        for seg in &segs {
            if cleaning_disk && free.is_some_and(|f| f >= TARGET_FREE) {
                cleaning_disk = false;
            }
            let over_quota = self.max_bytes > 0 && total > self.max_bytes;
            if !over_quota && !cleaning_disk {
                break;
            }
            if seg.end > busy {
                continue;
            }
            if std::fs::remove_file(&seg.path).is_ok() {
                total = total.saturating_sub(seg.size);
                free = free.map(|f| f + seg.size);
                removed += 1;
            }
        }

        // 4. 移除空的日期資料夾
        for cam in std::fs::read_dir(&self.root).into_iter().flatten().flatten() {
            for day in std::fs::read_dir(cam.path()).into_iter().flatten().flatten() {
                let empty = std::fs::read_dir(day.path()).map(|mut d| d.next().is_none()).unwrap_or(false);
                let is_today = day.file_name().to_string_lossy() == now.format("%Y-%m-%d").to_string();
                if empty && !is_today {
                    let _ = std::fs::remove_dir(day.path());
                }
            }
        }
        if removed > 0 {
            tracing::info!("錄影清理：刪除 {removed} 個片段");
        }
    }
}
