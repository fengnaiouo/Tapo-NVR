//! Tapo NVR 伺服器（可獨立執行，也可嵌入桌面版）。

#[macro_use]
pub mod i18n;
mod api;
pub mod auth;
pub mod config;
mod discovery;
mod events;
mod go2rtc;
mod notify;
mod onvif;
mod recorder;
mod recordings;
mod secret;
mod settings;
pub mod store;
mod tapo;
mod webassets;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicU64};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use axum::http::{header, HeaderValue};
use axum::routing::{delete, get, post, put};
use axum::{middleware, Router};
use serde_json::json;
use tokio::sync::{broadcast, Mutex, Notify};
use tokio_util::sync::CancellationToken;
use tower_http::set_header::SetResponseHeaderLayer;

use config::{CameraConfig, Config, ServerConfig};

/// 結束碼 3：要求外層（start.ps1 或桌面版）重新啟動伺服器
pub const EXIT_RESTART: i32 = 3;

pub use auth::hash_password;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

pub struct Inner {
    pub store: Arc<store::Store>,
    pub data_dir: PathBuf,
    /// go2rtc 本機 API / RTSP 的隨機帳密（每次啟動重新產生，避免其他程式讀取串流）
    pub go2rtc_user: String,
    pub go2rtc_pass: String,
    /// 目前傳給 go2rtc 的設定（不寫入磁碟，內含攝影機密碼）
    pub go2rtc_config: std::sync::Mutex<String>,
    pub go2rtc_bin: PathBuf,
    pub ffmpeg_bin: PathBuf,
    pub can_restart: bool,
    pub desktop_embedded: bool,
    startup_server: ServerConfig,
    startup_go2rtc_binary: PathBuf,
    cfg: RwLock<Arc<Config>>,
    cameras: RwLock<Arc<HashMap<String, Arc<CameraHandle>>>>,
    pub auth: auth::Auth,
    pub events: events::EventHub,
    pub recorders: recorder::Recorders,
    pub http: reqwest::Client,
    pub go2rtc_restart: Notify,
    pub shutdown: CancellationToken,
    pub exit_code: AtomicI32,
    watchers: Mutex<HashMap<String, (CameraConfig, CancellationToken)>>,
    apply_lock: Mutex<()>,
}

pub struct CameraHandle {
    pub cfg: CameraConfig,
    pub onvif: Arc<onvif::OnvifClient>,
    pub ptz_gen: Arc<AtomicU64>,
    /// Tapo 私有控制 API（需 TP-Link 雲端密碼）
    pub tapo: Option<Arc<tapo::TapoClient>>,
}

impl CameraHandle {
    fn new(c: &CameraConfig) -> Self {
        Self {
            cfg: c.clone(),
            onvif: Arc::new(onvif::OnvifClient::new(&c.host, c.onvif_port, &c.username, &c.password)),
            ptz_gen: Arc::new(AtomicU64::new(0)),
            tapo: (!c.cloud_password.is_empty()).then(|| Arc::new(tapo::TapoClient::new(&c.host, &c.cloud_password))),
        }
    }
}

impl AppState {
    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.read().unwrap().clone()
    }

    pub fn camera(&self, id: &str) -> Option<Arc<CameraHandle>> {
        self.cameras.read().unwrap().get(id).cloned()
    }

    pub fn go2rtc_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.cfg().go2rtc.api_port)
    }

    /// 這些設定只在啟動時讀取，變更後需重新啟動
    pub fn restart_required(&self) -> bool {
        let cfg = self.cfg();
        cfg.server != self.startup_server || cfg.go2rtc.binary != self.startup_go2rtc_binary
    }

    /// 修改設定 → 驗證 → 寫回 config.toml → 立即套用
    pub async fn update_config<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut Config) -> Result<()>,
    {
        let _guard = self.apply_lock.lock().await;
        let mut next = (*self.cfg()).clone();
        f(&mut next)?;
        next.validate()?;
        self.store.save_config(&next)?;
        self.apply(next).await;
        Ok(())
    }

    async fn apply(&self, next: Config) {
        let next = Arc::new(next);
        let old = self.cameras.read().unwrap().clone();
        let map: HashMap<_, _> = next
            .cameras
            .iter()
            .map(|c| {
                let handle = match old.get(&c.id) {
                    Some(h) if h.cfg == *c => h.clone(),
                    _ => Arc::new(CameraHandle::new(c)),
                };
                (c.id.clone(), handle)
            })
            .collect();
        *self.cameras.write().unwrap() = Arc::new(map);
        *self.cfg.write().unwrap() = next.clone();

        i18n::set_default(&next.notify.language);
        self.events.set_max_events(next.notify.max_events);
        let ids: Vec<String> = next.cameras.iter().map(|c| c.id.clone()).collect();
        self.events.retain_cameras(&ids).await;
        self.sync_watchers().await;
        recorder::sync(self).await;

        match go2rtc::write_config_if_changed(self, &next) {
            Ok(true) => {
                tracing::info!("串流設定已變更，重新啟動 go2rtc");
                self.go2rtc_restart.notify_one();
            }
            Ok(false) => {}
            Err(e) => tracing::error!("{e:#}"),
        }
        self.events.broadcast(json!({"type": "config"}));
    }

    /// 依設定啟動 / 停止 / 重啟各攝影機的移動偵測工作
    async fn sync_watchers(&self) {
        let cfg = self.cfg();
        let mut watchers = self.watchers.lock().await;
        let wanted: HashMap<&str, &CameraConfig> =
            cfg.cameras.iter().filter(|c| c.motion).map(|c| (c.id.as_str(), c)).collect();

        watchers.retain(|id, (c, token)| {
            let keep = wanted.get(id.as_str()).is_some_and(|w| *w == c);
            if !keep {
                token.cancel();
            }
            keep
        });
        for (id, cam) in wanted {
            if watchers.contains_key(id) {
                continue;
            }
            let Some(handle) = self.camera(id) else { continue };
            let token = CancellationToken::new();
            tokio::spawn(events::watch_camera(
                self.clone(),
                cam.clone(),
                handle.onvif.clone(),
                token.clone(),
            ));
            watchers.insert(id.to_string(), (cam.clone(), token));
        }
    }

    pub fn request_restart(&self) {
        self.exit_code.store(EXIT_RESTART, std::sync::atomic::Ordering::SeqCst);
        self.shutdown.cancel();
    }
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// 啟動選項
pub struct Options {
    /// 資料夾：tapo-nvr.db（設定與事件）、snapshots/
    pub data_dir: PathBuf,
    /// 舊版 config.toml：資料庫尚無設定時自動匯入，匯入後清除其中的密碼
    pub legacy_config: Option<PathBuf>,
    /// 覆寫 go2rtc 執行檔位置（桌面版使用內附檔案）
    pub go2rtc_bin: Option<PathBuf>,
    /// 覆寫 ffmpeg 位置；未指定時找 bin\ffmpeg.exe，再找 PATH
    pub ffmpeg_bin: Option<PathBuf>,
    /// 外層會在結束碼 3 時重啟（網頁上的「重新啟動」按鈕才會啟用）
    pub can_restart: bool,
    pub desktop_embedded: bool,
}

/// 執行中的伺服器
pub struct Server {
    state: AppState,
    serve_task: tokio::task::JoinHandle<Result<()>>,
    go2rtc_task: tokio::task::JoinHandle<()>,
    local_url: String,
}

impl Server {
    pub async fn start(opts: Options) -> Result<Server> {
        let data_dir = std::path::absolute(&opts.data_dir)?;
        let store = Arc::new(store::Store::open(&data_dir)?);
        store::migrate_legacy(&store, opts.legacy_config.as_deref(), &data_dir)?;
        let cfg = match store.load_config()? {
            Some(c) => c,
            None => {
                let c = Config::default();
                store.save_config(&c)?;
                tracing::info!("已建立設定資料庫 {}", store.path.display());
                c
            }
        };
        if let Err(e) = cfg.validate() {
            tracing::warn!("設定有誤：{e:#}");
        }
        i18n::set_default(&cfg.notify.language);
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let go2rtc_bin = opts.go2rtc_bin.clone().unwrap_or_else(|| {
            let p = resolve(&base, &cfg.go2rtc.binary);
            if p.exists() {
                return p;
            }
            // 獨立執行時也找執行檔旁邊的 bin 資料夾
            std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.join(&cfg.go2rtc.binary)))
                .filter(|p| p.exists())
                .unwrap_or(p)
        });
        let ffmpeg_bin = opts.ffmpeg_bin.clone().unwrap_or_else(|| {
            let local = go2rtc_bin.with_file_name("ffmpeg.exe");
            if local.exists() { local } else { PathBuf::from("ffmpeg") }
        });
        let web_dir = cfg.server.web_dir.as_ref().map(|d| resolve(&base, d));

        let state = AppState(Arc::new(Inner {
            auth: auth::Auth::new(store.clone()),
            events: events::EventHub::load(store.clone(), &data_dir, cfg.notify.max_events),
            store,
            go2rtc_user: auth::random_hex(8),
            go2rtc_pass: auth::random_hex(16),
            go2rtc_config: std::sync::Mutex::new(String::new()),
            go2rtc_bin,
            ffmpeg_bin,
            can_restart: opts.can_restart,
            desktop_embedded: opts.desktop_embedded,
            startup_server: cfg.server.clone(),
            startup_go2rtc_binary: cfg.go2rtc.binary.clone(),
            cameras: RwLock::new(Arc::new(
                cfg.cameras.iter().map(|c| (c.id.clone(), Arc::new(CameraHandle::new(c)))).collect(),
            )),
            recorders: recorder::Recorders::default(),
            http: reqwest::Client::new(),
            go2rtc_restart: Notify::new(),
            shutdown: CancellationToken::new(),
            exit_code: AtomicI32::new(0),
            watchers: Mutex::new(HashMap::new()),
            apply_lock: Mutex::new(()),
            data_dir,
            cfg: RwLock::new(Arc::new(cfg)),
        }));

        let listen = state.cfg().server.listen.clone();
        let listener = tokio::net::TcpListener::bind(&listen)
            .await
            .with_context(|| tr!("無法監聽 {listen}（連接埠可能已被其他程式占用）", "Cannot listen on {listen} (the port may be used by another program)"))?;
        let addr = listener.local_addr()?;
        let host = if addr.ip().is_unspecified() { "127.0.0.1".to_string() } else { addr.ip().to_string() };
        let local_url = format!("http://{host}:{}", addr.port());

        go2rtc::write_config_if_changed(&state, &state.cfg())?;
        let go2rtc_task = tokio::spawn(go2rtc::supervise(state.clone()));
        state.sync_watchers().await;
        recorder::sync(&state).await;
        tokio::spawn(recorder::retention_loop(state.clone()));

        let app = router(state.clone(), web_dir);
        let cfg = state.cfg();
        tracing::info!("Tapo NVR 已啟動：{local_url}（監聽 {listen}，{} 台攝影機）", cfg.cameras.len());
        if !cfg.auth.is_configured() {
            tracing::warn!("尚未設定管理員帳號，請在本機或區網以瀏覽器開啟上述網址完成首次設定");
        }

        let shutdown = state.shutdown.clone();
        let serve_task = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async move { shutdown.cancelled().await })
                .await
                .map_err(Into::into)
        });

        Ok(Server { state, serve_task, go2rtc_task, local_url })
    }

    pub fn local_url(&self) -> &str {
        &self.local_url
    }

    pub fn needs_setup(&self) -> bool {
        !self.state.cfg().auth.is_configured()
    }

    /// 本機桌面版自動登入：建立 Session 並回傳一次性票證
    pub async fn issue_ticket(&self) -> Option<String> {
        let cfg = self.state.cfg();
        if !cfg.auth.is_configured() {
            return None;
        }
        Some(self.state.auth.issue_ticket(&cfg.auth.username, cfg.auth.session_days).await)
    }

    /// 即時事件（JSON 字串），與網頁 /api/ws 相同格式
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.state.events.tx.subscribe()
    }

    pub fn snapshot_path(&self, name: &str) -> Option<PathBuf> {
        let ok = name.ends_with(".jpg")
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            && !name.contains("..");
        ok.then(|| self.state.events.snapshot_dir.join(name))
    }

    pub fn recording_path(&self) -> Option<PathBuf> {
        self.state.cfg().recording.root()
    }

    pub fn data_dir(&self) -> &Path {
        &self.state.data_dir
    }

    pub fn shutdown(&self) {
        self.state.shutdown.cancel();
    }

    pub fn shutdown_handle(&self) -> CancellationToken {
        self.state.shutdown.clone()
    }

    /// 等待伺服器結束，回傳結束碼（EXIT_RESTART = 要求重新啟動）
    pub async fn wait(self) -> i32 {
        let _ = self.serve_task.await;
        self.state.shutdown.cancel();
        recorder::stop_all(&self.state).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.go2rtc_task).await;
        self.state.exit_code.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn router(state: AppState, web_dir: Option<PathBuf>) -> Router {
    let protected = Router::new()
        .route("/api/me", get(auth::me))
        .route("/api/logout", post(auth::logout))
        .route("/api/auth/ticket", post(auth::create_ticket))
        .route("/api/cameras", get(api::list_cameras))
        .route("/api/cameras/{id}/snapshot", get(api::snapshot))
        .route("/api/cameras/{id}/ptz/move", post(api::ptz_move))
        .route("/api/cameras/{id}/ptz/stop", post(api::ptz_stop))
        .route("/api/cameras/{id}/presets", get(api::list_presets).post(api::save_preset))
        .route("/api/cameras/{id}/presets/{token}/goto", post(api::goto_preset))
        .route("/api/cameras/{id}/presets/{token}", delete(api::delete_preset))
        .route("/api/cameras/{id}/device", get(api::device_status).put(api::device_update))
        .route("/api/events", get(api::list_events))
        .route("/api/snapshots/{file}", get(api::event_snapshot))
        .route("/api/ws", get(events::events_ws))
        .route("/api/recordings/status", get(recordings::status))
        .route("/api/recordings/{id}/days", get(recordings::days))
        .route("/api/recordings/{id}/segments", get(recordings::segments))
        .route("/api/recordings/{id}/video/{date}/{file}", get(recordings::video))
        .route("/api/recordings/{id}/export", get(recordings::export))
        .route("/api/settings", get(settings::get_settings))
        .route("/api/settings/notify", put(settings::put_notify))
        .route("/api/settings/notify/test", post(notify::test))
        .route("/api/settings/recording", put(settings::put_recording))
        .route("/api/settings/browse", get(settings::browse))
        .route("/api/settings/mkdir", post(settings::mkdir))
        .route("/api/settings/server", put(settings::put_server))
        .route("/api/settings/account", put(settings::put_account))
        .route("/api/settings/cameras", post(settings::create_camera))
        .route("/api/settings/cameras/order", put(settings::reorder_cameras))
        .route("/api/settings/cameras/test", post(settings::test_camera_handler))
        .route("/api/settings/cameras/discover", post(settings::discover))
        .route(
            "/api/settings/cameras/{id}",
            put(settings::update_camera).delete(settings::delete_camera),
        )
        .route("/api/system/restart", post(settings::restart))
        .route("/go2rtc/api/ws", get(go2rtc::ws_proxy))
        .route("/go2rtc/{file}", get(go2rtc::player_js))
        .layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    let mut app = Router::new()
        .route("/api/login", post(auth::login))
        .route("/api/setup", get(auth::setup_status).post(auth::setup))
        .route("/api/i18n/zh-CN", get(webassets::zh_cn))
        .route("/auth/consume", get(auth::consume_ticket))
        .merge(protected);
    app = match web_dir {
        Some(dir) => app.fallback_service(tower_http::services::ServeDir::new(dir)),
        None => app.fallback(webassets::serve),
    };
    // 沒有指定快取策略的回應（API、ServeDir 靜態檔）一律要求重新驗證
    app.layer(SetResponseHeaderLayer::if_not_present(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    ))
    .layer(SetResponseHeaderLayer::overriding(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    ))
    .layer(SetResponseHeaderLayer::overriding(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("DENY"),
    ))
    .layer(middleware::from_fn(i18n::layer))
    .with_state(state)
}

/// 命令列 `check`：測試資料庫中每台攝影機
pub async fn check(data_dir: &Path) -> Result<()> {
    let cfg = read_config(data_dir)?;
    if cfg.cameras.is_empty() {
        println!("尚未設定任何攝影機");
    }
    for cam in &cfg.cameras {
        println!("== {} ({}) {}", cam.name, cam.id, cam.host);
        for line in settings::test_camera(cam).await.lines {
            println!("  {line}");
        }
    }
    Ok(())
}

/// 讀取資料庫中的設定（伺服器未執行時使用，例如桌面版的偏好設定頁）
pub fn read_config(data_dir: &Path) -> Result<Config> {
    Ok(store::Store::open(data_dir)?.load_config()?.unwrap_or_default())
}

/// 修改資料庫中的設定；伺服器執行中修改需重新啟動才會套用
pub fn modify_config(data_dir: &Path, f: impl FnOnce(&mut Config)) -> Result<()> {
    let store = store::Store::open(data_dir)?;
    let mut cfg = store.load_config()?.unwrap_or_default();
    f(&mut cfg);
    cfg.validate()?;
    store.save_config(&cfg)
}

/// 匯入舊版 config.toml，回傳攝影機數量（原檔不會修改）
pub fn import_toml(data_dir: &Path, path: &Path) -> Result<usize> {
    let cfg = Config::from_toml_file(path)?;
    store::Store::open(data_dir)?.save_config(&cfg)?;
    Ok(cfg.cameras.len())
}

/// 把自己放進「關閉時終止所有子程序」的 Job Object，程式被強制關閉時 go2rtc / ffmpeg 也會一起結束
#[cfg(windows)]
pub fn kill_children_on_exit() {
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        AssignProcessToJobObject(job, GetCurrentProcess());
        // 刻意不關閉 handle：程序結束時由系統關閉，觸發 KILL_ON_JOB_CLOSE
    }
}

#[cfg(not(windows))]
pub fn kill_children_on_exit() {}

/// 磁碟剩餘空間（位元組）
pub fn free_space(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut free: u64 = 0;
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}
