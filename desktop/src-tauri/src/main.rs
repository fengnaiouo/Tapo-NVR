#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod toast;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::{NewWindowResponse, PermissionKind, PermissionResponse};
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

const IDENTIFIER: &str = "com.tapo-nvr.desktop";
/// 預設更新來源：GitHub Releases 最新版的 latest.json（偏好設定可改）
const DEFAULT_UPDATE_URL: &str = "https://github.com/fengnaiouo/Tapo-NVR/releases/latest/download/latest.json";

use tapo_nvr::tr;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum Mode {
    /// 內建伺服器（預設）
    #[default]
    Local,
    /// 連線到其他電腦上的伺服器
    Remote,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    mode: Mode,
    server_url: String,
    token: Option<String>,
    notifications: bool,
    update_url: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { mode: Mode::Local, server_url: String::new(), token: None, notifications: true, update_url: String::new() }
    }
}

struct LocalInfo {
    url: String,
    shutdown: CancellationToken,
}

#[derive(Default)]
struct AppState {
    prefs: Mutex<Prefs>,
    /// 每次切換連線遞增，讓舊的通知工作自行結束
    generation: Mutex<u64>,
    local: Mutex<Option<LocalInfo>>,
    local_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    restart_local: AtomicBool,
    quitting: AtomicBool,
    last_error: Mutex<Option<String>>,
    /// 已經用通知提醒過的新版本
    update_notified: Mutex<Option<String>>,
}

// ---------------------------------------------------------------------------
// 路徑與偏好設定

fn data_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().expect("app data dir")
}

/// 伺服器資料夾：tapo-nvr.db（設定、事件，機密以 DPAPI 加密）與 snapshots/
fn server_data_dir(app: &AppHandle) -> PathBuf {
    data_dir(app).join("data")
}

fn prefs_path(app: &AppHandle) -> PathBuf {
    data_dir(app).join("desktop.json")
}

fn load_prefs(app: &AppHandle) -> Prefs {
    std::fs::read_to_string(prefs_path(app))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_prefs(app: &AppHandle) {
    let p = app.state::<AppState>().prefs.lock().unwrap().clone();
    let path = prefs_path(app);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, serde_json::to_string_pretty(&p).unwrap());
}

/// 內附檔案（go2rtc / ffmpeg）：安裝版在 resources，開發版在專案的 bin 資料夾
fn bundled(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join(name));
        candidates.push(dir.join("bin").join(name));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    candidates.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../bin")).join(name));
    candidates.into_iter().find(|p| p.exists())
}

fn app_page(path: &str) -> url::Url {
    let base = if cfg!(windows) { "http://tauri.localhost/" } else { "tauri://localhost/" };
    url::Url::parse(base).unwrap().join(path).unwrap()
}

/// 目前監控頁的網址（本機伺服器或遠端伺服器）
fn server_base(app: &AppHandle) -> Option<String> {
    let state = app.state::<AppState>();
    if let Some(l) = state.local.lock().unwrap().as_ref() {
        return Some(l.url.clone());
    }
    let p = state.prefs.lock().unwrap();
    (p.mode == Mode::Remote && !p.server_url.is_empty()).then(|| p.server_url.clone())
}

fn navigate(app: &AppHandle, url: &str) {
    if let (Some(w), Ok(u)) = (app.get_webview_window("main"), url::Url::parse(url)) {
        let _ = w.navigate(u);
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn show_error(app: &AppHandle, msg: &str) {
    tracing::error!("{msg}");
    *app.state::<AppState>().last_error.lock().unwrap() = Some(msg.to_string());
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.navigate(app_page("index.html?view=error"));
    }
    show_main(app);
}

/// 偏好設定留空時使用 GitHub Releases
fn effective_update_url(p: &Prefs) -> String {
    if p.update_url.trim().is_empty() { DEFAULT_UPDATE_URL.to_string() } else { p.update_url.trim().to_string() }
}

fn open_external(target: &str) {
    let _ = std::process::Command::new("explorer").arg(target).spawn();
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap()
}

// ---------------------------------------------------------------------------
// 內建伺服器

fn start_local(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut task = state.local_task.lock().unwrap();
    if task.as_ref().is_some_and(|t| !t.inner().is_finished()) {
        return;
    }
    let handle = app.clone();
    *task = Some(tauri::async_runtime::spawn(async move { run_local(handle).await }));
}

/// 重新啟動內建伺服器（套用匯入的設定檔、連接埠等）
fn restart_local(app: &AppHandle) {
    let state = app.state::<AppState>();
    let running = state.local.lock().unwrap().as_ref().map(|l| l.shutdown.clone());
    match running {
        Some(token) => {
            state.restart_local.store(true, Ordering::SeqCst);
            token.cancel();
        }
        None => start_local(app),
    }
}

async fn stop_local(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Some(l) = state.local.lock().unwrap().as_ref() {
        l.shutdown.cancel();
    }
    let task = state.local_task.lock().unwrap().take();
    if let Some(t) = task {
        let _ = tokio::time::timeout(Duration::from_secs(8), t).await;
    }
}

async fn run_local(app: AppHandle) {
    loop {
        let go2rtc = bundled(&app, "go2rtc.exe");
        if go2rtc.is_none() {
            show_error(&app, &tr!("找不到內附的 go2rtc.exe，請重新安裝 Tapo NVR。", "The bundled go2rtc.exe is missing. Please reinstall Tapo NVR."));
            return;
        }
        let opts = tapo_nvr::Options {
            data_dir: server_data_dir(&app),
            // 0.2.x 版的設定檔：第一次啟動時自動匯入資料庫並清除其中的密碼
            legacy_config: Some(data_dir(&app).join("config.toml")),
            go2rtc_bin: go2rtc,
            ffmpeg_bin: bundled(&app, "ffmpeg.exe"),
            can_restart: true,
            desktop_embedded: true,
        };
        let server = match tapo_nvr::Server::start(opts).await {
            Ok(s) => s,
            Err(e) => {
                show_error(&app, &format!("{e:#}"));
                return;
            }
        };
        let url = server.local_url().to_string();
        let snapshots = server.data_dir().join("snapshots");
        {
            let state = app.state::<AppState>();
            *state.local.lock().unwrap() = Some(LocalInfo {
                url: url.clone(),
                shutdown: server.shutdown_handle(),
            });
            *state.last_error.lock().unwrap() = None;
        }

        // 通知：直接訂閱伺服器內部的事件，不經過網路
        let gen = bump_generation(&app);
        let mut rx = server.subscribe();
        let h = app.clone();
        let snap_dir = snapshots.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(text) => {
                        if current_generation(&h) != gen {
                            return;
                        }
                        handle_event(&h, &text, Source::Local(snap_dir.clone())).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                }
            }
        });

        let target = if server.needs_setup() {
            format!("{url}/setup.html")
        } else {
            match server.issue_ticket().await {
                Some(t) => format!("{url}/auth/consume?ticket={t}&client=desktop"),
                None => format!("{url}/"),
            }
        };
        navigate(&app, &target);

        let code = server.wait().await;
        let state = app.state::<AppState>();
        *state.local.lock().unwrap() = None;
        let again = code == tapo_nvr::EXIT_RESTART || state.restart_local.swap(false, Ordering::SeqCst);
        if !again || state.quitting.load(Ordering::SeqCst) {
            return;
        }
        tracing::info!("重新啟動內建伺服器");
    }
}

// ---------------------------------------------------------------------------
// 遠端伺服器

async fn open_remote(app: &AppHandle) -> Result<(), String> {
    let s = app.state::<AppState>().prefs.lock().unwrap().clone();
    let token = s.token.clone().ok_or_else(|| tr!("尚未登入", "Not signed in"))?;
    let resp = http()
        .post(format!("{}/api/auth/ticket", s.server_url))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| tr!("無法連線伺服器：{e}", "Cannot reach the server: {e}"))?;
    if !resp.status().is_success() {
        return Err(tr!("登入已過期，請重新登入", "Session expired, please sign in again"));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let ticket = body["ticket"].as_str().ok_or_else(|| tr!("伺服器回應格式錯誤", "Unexpected server response"))?;
    navigate(app, &format!("{}/auth/consume?ticket={ticket}&client=desktop", s.server_url));

    let gen = bump_generation(app);
    let h = app.clone();
    tauri::async_runtime::spawn(async move { remote_notifier(h, s.server_url, token, gen).await });
    Ok(())
}

async fn remote_notifier(app: AppHandle, base: String, token: String, gen: u64) {
    let ws_url = format!("ws{}/api/ws", base.strip_prefix("http").unwrap_or(&base));
    let mut delay = 2u64;
    loop {
        if current_generation(&app) != gen {
            return;
        }
        let Ok(mut req) = ws_url.as_str().into_client_request() else { return };
        req.headers_mut().insert("Authorization", format!("Bearer {token}").parse().unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Ok((mut ws, _)) => {
                delay = 2;
                while let Some(Ok(msg)) = ws.next().await {
                    if current_generation(&app) != gen {
                        return;
                    }
                    if let Message::Text(t) = msg {
                        handle_event(&app, t.as_str(), Source::Remote(base.clone(), token.clone())).await;
                    }
                }
            }
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == 401 => return,
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(60);
    }
}

fn bump_generation(app: &AppHandle) -> u64 {
    let state = app.state::<AppState>();
    let mut g = state.generation.lock().unwrap();
    *g += 1;
    *g
}

fn current_generation(app: &AppHandle) -> u64 {
    *app.state::<AppState>().generation.lock().unwrap()
}

// ---------------------------------------------------------------------------
// 通知

enum Source {
    Local(PathBuf),
    Remote(String, String),
}

async fn handle_event(app: &AppHandle, text: &str, source: Source) {
    let Ok(msg) = serde_json::from_str::<serde_json::Value>(text) else { return };
    if msg["type"] != "event" {
        return;
    }
    let ev = &msg["event"];
    if ev["notify"] == false || !app.state::<AppState>().prefs.lock().unwrap().notifications {
        return;
    }
    let kind = match ev["kind"].as_str().unwrap_or("") {
        "motion" => tr!("移動偵測", "Motion detected"),
        "person" => tr!("偵測到人形", "Person detected"),
        "tamper" => tr!("鏡頭遭遮蔽", "Camera tampering"),
        other => other.to_string(),
    };
    let camera_name = ev["camera_name"].as_str().map(str::to_string).unwrap_or_else(|| tr!("攝影機", "Camera"));
    let camera_id = ev["camera"].as_str().unwrap_or("").to_string();
    let time = ev["ts"]
        .as_str()
        .and_then(|t| chrono_like_local(t))
        .unwrap_or_default();
    let snapshot = ev["snapshot"].as_str().filter(|s| safe_name(s)).map(str::to_string);

    let image = match (&source, snapshot) {
        (Source::Local(dir), Some(name)) => Some(dir.join(name)),
        (Source::Remote(base, token), Some(name)) => download_snapshot(app, base, token, &name).await,
        _ => None,
    };
    let h = app.clone();
    toast::show(toast::Request {
        title: tr!("{camera_name}：{kind}", "{camera_name}: {kind}"),
        body: time,
        image,
        on_click: Box::new(move || {
            let app = h.clone();
            let id = camera_id.clone();
            let _ = h.run_on_main_thread(move || open_camera(&app, &id));
        }),
    });
}

fn safe_name(s: &str) -> bool {
    s.ends_with(".jpg") && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) && !s.contains("..")
}

/// 「2026-10-29T02:37:14.26Z」→ 本地時間「10:37:14」
fn chrono_like_local(ts: &str) -> Option<String> {
    let parsed = ts.parse::<chrono::DateTime<chrono::Utc>>().ok()?;
    Some(parsed.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
}

async fn download_snapshot(app: &AppHandle, base: &str, token: &str, name: &str) -> Option<PathBuf> {
    let dir = app.path().app_cache_dir().ok()?.join("toasts");
    std::fs::create_dir_all(&dir).ok()?;
    let bytes = http()
        .get(format!("{base}/api/snapshots/{name}"))
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()?;
    let path = dir.join(name);
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// 點通知：叫出視窗並開啟該攝影機的放大畫面
fn open_camera(app: &AppHandle, id: &str) {
    show_main(app);
    let Some(base) = server_base(app) else { return };
    let Some(w) = app.get_webview_window("main") else { return };
    let on_live_page = w
        .url()
        .ok()
        .is_some_and(|u| base.starts_with(u.origin().ascii_serialization().as_str()) && u.path() == "/");
    let id_js = serde_json::to_string(id).unwrap();
    if on_live_page {
        let _ = w.eval(format!("window.tapoOpenCamera && window.tapoOpenCamera({id_js})"));
    } else if tapo_nvr::config::valid_camera_id(id) {
        navigate(app, &format!("{base}/?camera={id}"));
    }
}

// ---------------------------------------------------------------------------
// 網頁 → 桌面指令（攔截導向 https://tapo-app.invalid/cmd/...，不開放 IPC 給網頁）

fn run_command(app: &AppHandle, cmd: &str) {
    let Some(w) = app.get_webview_window("main") else { return };
    match cmd {
        "fullscreen-on" => {
            let _ = w.set_fullscreen(true);
        }
        "fullscreen-off" => {
            let _ = w.set_fullscreen(false);
        }
        "open-recordings" => open_recordings(app),
        _ => tracing::warn!("未知的桌面指令：{cmd}"),
    }
}

fn open_recordings(app: &AppHandle) {
    let root = tapo_nvr::read_config(&server_data_dir(app)).ok().and_then(|c| c.recording.root());
    match root {
        Some(r) if r.exists() => open_external(&r.to_string_lossy()),
        _ => {
            app.dialog()
                .message(tr!("尚未設定錄影資料夾。請到「設定 → 錄影」選擇。", "No recording folder yet. Choose one in Settings → Recording."))
                .title("Tapo NVR")
                .kind(MessageDialogKind::Info)
                .show(|_| {});
        }
    }
}

// ---------------------------------------------------------------------------
// 自動更新

/// 背景檢查：發現新版本時只顯示 Windows 通知（同一版本只提醒一次），點擊通知才詢問是否更新，
/// 避免在使用者全螢幕（例如玩遊戲）時跳出對話框搶走焦點
async fn background_update_check(app: &AppHandle) {
    let url = effective_update_url(&app.state::<AppState>().prefs.lock().unwrap());
    let Ok(endpoint) = url::Url::parse(url.trim()) else { return };
    let Ok(updater) = app.updater_builder().endpoints(vec![endpoint]).and_then(|b| b.build()) else { return };
    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            {
                let state = app.state::<AppState>();
                let mut seen = state.update_notified.lock().unwrap();
                if seen.as_deref() == Some(version.as_str()) {
                    return;
                }
                *seen = Some(version.clone());
            }
            tracing::info!("發現新版本 {version}");
            let h = app.clone();
            toast::show(toast::Request {
                title: tr!("Tapo NVR {version} 已推出", "Tapo NVR {version} is available"),
                body: tr!("點這裡更新，或從系統匣選單 →「檢查更新」", "Click to update, or use the tray menu → Check for updates"),
                image: None,
                on_click: Box::new(move || {
                    let h = h.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = check_update(&h, true).await {
                            h.dialog().message(e).title("Tapo NVR").kind(MessageDialogKind::Warning).show(|_| {});
                        }
                    });
                }),
            });
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("檢查更新失敗：{e}"),
    }
}

async fn check_update(app: &AppHandle, interactive: bool) -> Result<String, String> {
    let url = effective_update_url(&app.state::<AppState>().prefs.lock().unwrap());
    let endpoint = url::Url::parse(url.trim()).map_err(|_| tr!("更新來源網址格式錯誤", "Invalid update URL"))?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let notes = update.body.clone().unwrap_or_default();
            let h = app.clone();
            let current = update.current_version.clone();
            let msg = tr!("發現新版本 {version}（目前 {current}）\n\n{notes}\n\n要立即下載並安裝嗎？安裝時程式會自動重新啟動。", "Version {version} is available (you have {current}).\n\n{notes}\n\nDownload and install now? Tapo NVR will restart.");
            let yes = tauri::async_runtime::spawn_blocking(move || {
                h.dialog()
                    .message(msg)
                    .title(tr!("Tapo NVR 更新", "Tapo NVR update"))
                    .buttons(MessageDialogButtons::OkCancelCustom(tr!("立即更新", "Update now"), tr!("稍後", "Later")))
                    .blocking_show()
            })
            .await
            .unwrap_or(false);
            if yes {
                toast::show(toast::Request {
                    title: tr!("正在下載 Tapo NVR {version}", "Downloading Tapo NVR {version}"),
                    body: tr!("下載完成後會自動安裝並重新啟動", "It will install and restart automatically"),
                    image: None,
                    on_click: Box::new(|| {}),
                });
                stop_local(app).await;
                update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
                app.restart();
            }
            Ok(tr!("發現新版本 {version}", "Version {version} is available"))
        }
        Ok(None) => {
            if interactive {
                app.dialog().message(tr!("目前已是最新版本。", "You are on the latest version.")).title("Tapo NVR").show(|_| {});
            }
            Ok(tr!("目前已是最新版本", "You are on the latest version"))
        }
        Err(e) => Err(tr!("檢查更新失敗：{e}", "Update check failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// 偏好設定頁（內建頁面，透過 IPC 呼叫）

#[derive(Serialize)]
struct PrefsView {
    mode: Mode,
    server_url: String,
    has_token: bool,
    notifications: bool,
    update_url: String,
    default_update_url: String,
    lang: String,
    version: String,
    config_path: String,
    data_dir: String,
    local_running: bool,
    error: Option<String>,
}

#[tauri::command]
fn get_prefs(app: AppHandle, state: State<AppState>) -> PrefsView {
    let p = state.prefs.lock().unwrap().clone();
    PrefsView {
        mode: p.mode,
        server_url: p.server_url,
        has_token: p.token.is_some(),
        notifications: p.notifications,
        update_url: p.update_url,
        default_update_url: DEFAULT_UPDATE_URL.into(),
        lang: tapo_nvr::i18n::current().code().into(),
        version: app.package_info().version.to_string(),
        config_path: server_data_dir(&app).join(tapo_nvr::store::DB_FILE).display().to_string(),
        data_dir: data_dir(&app).display().to_string(),
        local_running: state.local.lock().unwrap().is_some(),
        error: state.last_error.lock().unwrap().clone(),
    }
}

#[tauri::command]
fn set_update_url(app: AppHandle, state: State<AppState>, url: String) -> Result<(), String> {
    let url = url.trim().to_string();
    if !url.is_empty() && !url.starts_with("https://") {
        return Err(tr!("更新來源必須是 https:// 網址", "The update URL must start with https://"));
    }
    state.prefs.lock().unwrap().update_url = url;
    save_prefs(&app);
    Ok(())
}

#[tauri::command]
async fn use_local(app: AppHandle) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        let mut p = state.prefs.lock().unwrap();
        p.mode = Mode::Local;
    }
    save_prefs(&app);
    bump_generation(&app);
    if server_base(&app).is_some() && app.state::<AppState>().local.lock().unwrap().is_some() {
        restart_local(&app);
    } else {
        start_local(&app);
    }
    Ok(())
}

#[tauri::command]
async fn connect_remote(app: AppHandle, server_url: String, username: String, password: String) -> Result<(), String> {
    let raw = server_url.trim().trim_end_matches('/');
    let base = if raw.starts_with("http://") || raw.starts_with("https://") { raw.to_string() } else { format!("http://{raw}") };
    url::Url::parse(&base).map_err(|_| tr!("伺服器網址格式錯誤", "Invalid server URL"))?;
    let resp = http()
        .post(format!("{base}/api/login"))
        .json(&serde_json::json!({"username": username, "password": password}))
        .send()
        .await
        .map_err(|e| tr!("無法連線伺服器：{e}", "Cannot reach the server: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        return Err(body["error"].as_str().map(str::to_string).unwrap_or_else(|| tr!("登入失敗", "Sign-in failed")));
    }
    let token = body["token"].as_str().ok_or_else(|| tr!("伺服器回應格式錯誤", "Unexpected server response"))?.to_string();
    {
        let state = app.state::<AppState>();
        let mut p = state.prefs.lock().unwrap();
        p.mode = Mode::Remote;
        p.server_url = base;
        p.token = Some(token);
    }
    save_prefs(&app);
    // 連到其他電腦時停止本機伺服器，避免重複錄影
    stop_local(&app).await;
    open_remote(&app).await
}

#[tauri::command]
async fn retry(app: AppHandle) -> Result<(), String> {
    let mode = app.state::<AppState>().prefs.lock().unwrap().mode;
    match mode {
        Mode::Local => {
            start_local(&app);
            Ok(())
        }
        Mode::Remote => open_remote(&app).await,
    }
}

/// 匯入既有的 config.toml（例如先前用 start.bat 執行時的設定）
#[tauri::command]
async fn import_config(app: AppHandle) -> Result<String, String> {
    let h = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        h.dialog().file().set_title(tr!("選擇要匯入的 config.toml", "Choose a config.toml to import")).add_filter(tr!("Tapo NVR 設定檔", "Tapo NVR config"), &["toml"]).blocking_pick_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(file) = picked else { return Err(tr!("已取消", "Cancelled")) };
    let src = file.into_path().map_err(|e| e.to_string())?;
    let n = tapo_nvr::import_toml(&server_data_dir(&app), &src).map_err(|e| format!("{e:#}"))?;
    restart_local(&app);
    Ok(tr!("已匯入 {} 台攝影機", "Imported {} cameras", n))
}

/// 連接埠被占用時改用其他位址
#[tauri::command]
async fn set_listen(app: AppHandle, listen: String) -> Result<(), String> {
    let listen = listen.trim().to_string();
    listen.parse::<std::net::SocketAddr>().map_err(|_| tr!("格式錯誤，例如 0.0.0.0:8090", "Invalid format, e.g. 0.0.0.0:8090"))?;
    tapo_nvr::modify_config(&server_data_dir(&app), |c| c.server.listen = listen).map_err(|e| format!("{e:#}"))?;
    restart_local(&app);
    Ok(())
}

#[tauri::command]
async fn check_update_cmd(app: AppHandle) -> Result<String, String> {
    check_update(&app, false).await
}

#[tauri::command]
fn open_data_dir(app: AppHandle) {
    let dir = data_dir(&app);
    let _ = std::fs::create_dir_all(&dir);
    open_external(&dir.to_string_lossy());
}

#[tauri::command]
async fn back_to_app(app: AppHandle) -> Result<(), String> {
    match server_base(&app) {
        Some(base) => {
            navigate(&app, &format!("{base}/"));
            Ok(())
        }
        None => retry(app).await,
    }
}

// ---------------------------------------------------------------------------

fn init_logging(app: &AppHandle) {
    let dir = app.path().app_log_dir().unwrap_or_else(|_| data_dir(app).join("logs"));
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("tapo-nvr.log"));
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=warn".into());
    match file {
        Ok(f) => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Mutex::new(f)).try_init();
        }
        Err(_) => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    }
}

fn build_tray(app: &AppHandle, notifications: bool) -> tauri::Result<()> {
    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    let show = MenuItem::with_id(app, "show", tr!("顯示 Tapo NVR", "Show Tapo NVR"), true, None::<&str>)?;
    let playback = MenuItem::with_id(app, "playback", tr!("錄影回放", "Playback"), true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", tr!("設定", "Settings"), true, None::<&str>)?;
    let notify = CheckMenuItem::with_id(app, "notify", tr!("桌面通知", "Desktop notifications"), true, notifications, None::<&str>)?;
    let auto = CheckMenuItem::with_id(app, "autostart", tr!("開機時自動啟動", "Start with Windows"), true, autostart, None::<&str>)?;
    let rec = MenuItem::with_id(app, "recordings", tr!("開啟錄影資料夾", "Open recordings folder"), true, None::<&str>)?;
    let prefs = MenuItem::with_id(app, "prefs", tr!("應用程式偏好設定…", "App preferences…"), true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", tr!("檢查更新", "Check for updates"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", tr!("結束", "Quit"), true, None::<&str>)?;
    let s1 = PredefinedMenuItem::separator(app)?;
    let s2 = PredefinedMenuItem::separator(app)?;
    let s3 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&show, &playback, &settings, &s1, &notify, &auto, &s2, &rec, &prefs, &update, &s3, &quit],
    )?;
    let notify_item = notify.clone();
    let auto_item = auto.clone();
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Tapo NVR")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, e| match e.id.as_ref() {
            "show" => show_main(app),
            "playback" | "settings" => {
                if let Some(base) = server_base(app) {
                    let page = if e.id.as_ref() == "playback" { "playback.html" } else { "settings.html" };
                    navigate(app, &format!("{base}/{page}"));
                }
                show_main(app);
            }
            "notify" => {
                let on = notify_item.is_checked().unwrap_or(true);
                app.state::<AppState>().prefs.lock().unwrap().notifications = on;
                save_prefs(app);
            }
            "autostart" => {
                let on = auto_item.is_checked().unwrap_or(false);
                let launcher = app.autolaunch();
                let r = if on { launcher.enable() } else { launcher.disable() };
                if r.is_err() {
                    let _ = auto_item.set_checked(launcher.is_enabled().unwrap_or(false));
                }
            }
            "recordings" => open_recordings(app),
            "prefs" => {
                navigate(app, app_page("index.html?view=prefs").as_str());
                show_main(app);
            }
            "update" => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = check_update(&h, true).await {
                        h.dialog().message(e).title("Tapo NVR").kind(MessageDialogKind::Warning).show(|_| {});
                    }
                });
            }
            "quit" => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move {
                    h.state::<AppState>().quitting.store(true, Ordering::SeqCst);
                    stop_local(&h).await;
                    h.exit(0);
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    tapo_nvr::kill_children_on_exit();
    let start_hidden = std::env::args().any(|a| a == "--hidden");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show_main(app)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_prefs,
            set_update_url,
            use_local,
            connect_remote,
            retry,
            import_config,
            set_listen,
            check_update_cmd,
            open_data_dir,
            back_to_app
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            init_logging(&handle);
            toast::init(IDENTIFIER);
            let prefs = load_prefs(&handle);
            let mode = prefs.mode;
            let has_token = prefs.token.is_some();
            let notifications = prefs.notifications;
            let update_url = effective_update_url(&prefs);
            *app.state::<AppState>().prefs.lock().unwrap() = prefs;

            let nav = handle.clone();
            let newwin = handle.clone();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Tapo NVR")
                .inner_size(1280.0, 800.0)
                .min_inner_size(640.0, 420.0)
                .visible(!start_hidden)
                .initialization_script("try { localStorage.setItem('client', 'desktop'); } catch (e) {}")
                .on_navigation(move |url| {
                    if url.host_str() == Some("tapo-app.invalid") {
                        let cmd = url.path().trim_start_matches("/cmd/").to_string();
                        let app = nav.clone();
                        let _ = nav.run_on_main_thread(move || run_command(&app, &cmd));
                        return false;
                    }
                    true
                })
                .on_new_window(move |url, _| {
                    let _ = &newwin;
                    if matches!(url.scheme(), "http" | "https") {
                        open_external(url.as_str());
                    }
                    NewWindowResponse::Deny
                })
                .on_permission_request(|_, kind| match kind {
                    PermissionKind::Microphone | PermissionKind::Autoplay => PermissionResponse::Allow,
                    // 通知由原生端處理
                    PermissionKind::Notifications => PermissionResponse::Deny,
                    _ => PermissionResponse::Default,
                })
                .build()?;

            build_tray(&handle, notifications)?;

            match mode {
                Mode::Local => start_local(&handle),
                Mode::Remote if has_token => {
                    let h = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = open_remote(&h).await {
                            show_error(&h, &e);
                        }
                    });
                }
                Mode::Remote => {}
            }

            // 啟動 30 秒後檢查更新，之後每 6 小時檢查一次（程式通常常駐系統匣很多天）
            if !update_url.is_empty() {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    loop {
                        background_update_check(&h).await;
                        tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // 關閉視窗時縮到系統匣：伺服器、錄影與通知繼續在背景運作
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.set_fullscreen(false);
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("無法啟動 Tapo NVR");

    app.run(|app, event| {
        if let RunEvent::ExitRequested { .. } = event {
            let state = app.state::<AppState>();
            state.quitting.store(true, Ordering::SeqCst);
            let token = state.local.lock().unwrap().as_ref().map(|l| l.shutdown.clone());
            if let Some(t) = token {
                t.cancel();
            }
        }
    });
}
