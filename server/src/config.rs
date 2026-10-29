use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub go2rtc: Go2rtcConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub recording: RecordingConfig,
    #[serde(default)]
    pub update: UpdateConfig,
    #[serde(default)]
    pub cameras: Vec<CameraConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub listen: String,
    pub data_dir: PathBuf,
    /// 未設定時使用內建於程式中的網頁介面
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_dir: Option<PathBuf>,
    /// 經由 cloudflared 等反向代理時設為 true，才會信任 CF-Connecting-IP / X-Forwarded-* 標頭
    pub trust_proxy_headers: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".into(),
            data_dir: "data".into(),
            web_dir: None,
            trust_proxy_headers: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub username: String,
    /// 空字串代表尚未設定，伺服器會進入首次設定模式
    pub password_hash: String,
    pub session_days: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self { username: "admin".into(), password_hash: String::new(), session_days: 30 }
    }
}

impl AuthConfig {
    pub fn is_configured(&self) -> bool {
        self.password_hash.starts_with("$argon2")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Go2rtcConfig {
    pub binary: PathBuf,
    pub api_port: u16,
    /// go2rtc 內部 RTSP 轉送（錄影用，只綁定 127.0.0.1）
    pub rtsp_port: u16,
    pub webrtc_port: u16,
    /// 額外的 WebRTC candidate，例如 "stun:8555" 或 "203.0.113.10:8555"
    pub webrtc_candidates: Vec<String>,
}

impl Default for Go2rtcConfig {
    fn default() -> Self {
        Self {
            binary: "bin/go2rtc.exe".into(),
            api_port: 1984,
            rtsp_port: 8554,
            webrtc_port: 8555,
            webrtc_candidates: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    /// 總開關：關閉後仍會記錄事件，但不推送任何通知
    pub enabled: bool,
    /// 同一台攝影機同類事件兩次通知之間的最短間隔（秒）
    pub cooldown_secs: u64,
    pub snapshot: bool,
    /// 保留的事件數量上限（含截圖）
    pub max_events: usize,
    /// ntfy 手機推播：主題留空 = 停用
    pub ntfy_url: String,
    pub ntfy_topic: String,
    pub ntfy_token: String,
    /// 對外網址（例如 Cloudflare Tunnel），點推播時開啟
    pub public_url: String,
    /// 推播與背景訊息的語言：auto（系統語言）/ zh-TW / zh-CN / en
    pub language: String,
    /// 通知時段（布防排程）：啟用後只在指定星期與時間內推送通知；事件與錄影不受影響
    pub schedule_enabled: bool,
    /// "HH:MM"，結束早於開始代表跨夜（例如 22:00–07:00）
    pub schedule_start: String,
    pub schedule_end: String,
    /// 0 = 星期日 … 6 = 星期六
    pub schedule_days: Vec<u8>,
    /// 攝影機離線 / 恢復連線時通知
    pub offline_alert: bool,
}

impl NotifyConfig {
    /// 現在是否在通知時段內
    pub fn in_schedule(&self, now: chrono::DateTime<chrono::Local>) -> bool {
        use chrono::{Datelike, Timelike};
        if !self.schedule_enabled {
            return true;
        }
        let parse = |s: &str| -> Option<u32> {
            let (h, m) = s.trim().split_once(':')?;
            let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
            (h < 24 && m < 60).then_some(h * 60 + m)
        };
        let (Some(start), Some(end)) = (parse(&self.schedule_start), parse(&self.schedule_end)) else { return true };
        let minute = now.hour() * 60 + now.minute();
        let today = now.weekday().num_days_from_sunday() as u8;
        let yesterday = (today + 6) % 7;
        if start == end {
            return self.schedule_days.contains(&today);
        }
        if start < end {
            self.schedule_days.contains(&today) && minute >= start && minute < end
        } else {
            // 跨夜：開始那天的晚上，或隔天清晨（以開始那天為準）
            (minute >= start && self.schedule_days.contains(&today))
                || (minute < end && self.schedule_days.contains(&yesterday))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    /// 定期檢查 GitHub 上是否有新版本
    pub auto_check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self { auto_check: true }
    }
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_secs: 60,
            snapshot: true,
            max_events: 1000,
            ntfy_url: "https://ntfy.sh".into(),
            ntfy_topic: String::new(),
            ntfy_token: String::new(),
            public_url: String::new(),
            language: "auto".into(),
            schedule_enabled: false,
            schedule_start: "22:00".into(),
            schedule_end: "07:00".into(),
            schedule_days: vec![0, 1, 2, 3, 4, 5, 6],
            offline_alert: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    /// 錄影資料夾（絕對路徑）；留空 = 停用錄影，需在設定頁選擇
    pub path: String,
    /// 保留天數，0 = 不限
    pub max_days: u32,
    /// 總容量上限（GB），0 = 不限
    pub max_gb: u64,
    /// 「事件錄影」保留移動前後的秒數
    pub pre_seconds: u64,
    pub post_seconds: u64,
    /// 每個影片檔的長度（秒）
    pub segment_seconds: u64,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            max_days: 7,
            max_gb: 100,
            pre_seconds: 10,
            post_seconds: 30,
            segment_seconds: 60,
        }
    }
}

impl RecordingConfig {
    pub fn root(&self) -> Option<PathBuf> {
        (!self.path.trim().is_empty()).then(|| PathBuf::from(self.path.trim()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RecordMode {
    Off,
    Continuous,
    #[default]
    Events,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraConfig {
    pub id: String,
    pub name: String,
    pub host: String,
    /// Tapo App「攝影機帳戶」的帳號密碼（非 TP-Link 雲端帳號）
    pub username: String,
    pub password: String,
    #[serde(default = "default_rtsp_port")]
    pub rtsp_port: u16,
    #[serde(default = "default_onvif_port")]
    pub onvif_port: u16,
    #[serde(default = "yes")]
    pub ptz: bool,
    #[serde(default = "yes")]
    pub motion: bool,
    #[serde(default = "yes")]
    pub notify: bool,
    #[serde(default)]
    pub record: RecordMode,
    /// TP-Link 雲端密碼（選填）：攝影機控制、雙向語音、Tapo 私有串流需要
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cloud_password: String,
    /// 影像來源：rtsp（標準，需攝影機帳戶）或 tapo（私有協定，需雲端密碼；適用沒有 RTSP 的機型）
    #[serde(default)]
    pub stream: StreamSource,
    /// 機型（掃描或測試連線時自動填入，僅供顯示）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StreamSource {
    #[default]
    Rtsp,
    Tapo,
}

pub fn default_rtsp_port() -> u16 {
    554
}
pub fn default_onvif_port() -> u16 {
    2020
}
fn yes() -> bool {
    true
}

impl CameraConfig {
    pub fn main_stream(&self) -> String {
        format!("{}_main", self.id)
    }
    pub fn sub_stream(&self) -> String {
        format!("{}_sub", self.id)
    }
    pub fn talk_stream(&self) -> Option<String> {
        (!self.cloud_password.is_empty()).then(|| format!("{}_talk", self.id))
    }
    pub fn tapo_url(&self) -> String {
        format!("tapo://{}@{}", url_encode(&self.cloud_password), self.host)
    }
    /// go2rtc 的 Tapo 私有串流來源（新韌體需 SHA256，舊韌體用 MD5，兩個都列讓 go2rtc 依序嘗試）
    pub fn tapo_sources(&self, sub: bool) -> Vec<String> {
        let q = if sub { "?subtype=1" } else { "" };
        let sha = crate::tapo::sha256_upper(&[self.cloud_password.as_bytes()]);
        let md5: String = md5::compute(self.cloud_password.as_bytes()).0.iter().map(|b| format!("{b:02X}")).collect();
        vec![
            format!("tapo://admin:{sha}@{}{q}", self.host),
            format!("tapo://admin:{md5}@{}{q}", self.host),
        ]
    }
    pub fn rtsp_url(&self, path: &str) -> String {
        format!(
            "rtsp://{}:{}@{}:{}/{}",
            url_encode(&self.username),
            url_encode(&self.password),
            self.host,
            self.rtsp_port,
            path
        )
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn valid_camera_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl Config {
    /// 讀取舊版 config.toml（僅用於匯入；設定現在存在 SQLite 資料庫）
    pub fn from_toml_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| tr!("無法讀取設定檔 {}", "Cannot read config file {}", path.display()))?;
        let text = text.trim_start_matches('\u{feff}');
        let mut cfg: Config = toml::from_str(text).context(tr!("設定檔格式錯誤", "Invalid config file format"))?;
        // 桌面版 / 新版使用內建網頁與資料庫，舊設定的路徑不再適用
        cfg.server.web_dir = None;
        cfg.validate()?;
        Ok(cfg)
    }
    pub fn validate(&self) -> Result<()> {
        if !self.auth.password_hash.is_empty() && !self.auth.is_configured() {
            bail!("{}", tr!("auth.password_hash 格式錯誤（必須是 argon2 雜湊）", "auth.password_hash is invalid (must be an argon2 hash)"));
        }
        if self.auth.username.trim().is_empty() {
            bail!("{}", tr!("登入帳號不可空白", "Username cannot be empty"));
        }
        if !(1..=3650).contains(&self.auth.session_days) {
            bail!("{}", tr!("登入有效天數需介於 1–3650", "Session days must be between 1 and 3650"));
        }
        if self.server.listen.parse::<std::net::SocketAddr>().is_err() {
            bail!("{}", tr!("監聽位址格式錯誤，例如 0.0.0.0:8080", "Invalid listen address, e.g. 0.0.0.0:8080"));
        }
        let ports = [self.go2rtc.api_port, self.go2rtc.rtsp_port, self.go2rtc.webrtc_port];
        if ports.contains(&0) {
            bail!("{}", tr!("go2rtc 連接埠不可為 0", "go2rtc ports cannot be 0"));
        }
        let listen_port = self.server.listen.parse::<std::net::SocketAddr>().map(|a| a.port()).unwrap_or(0);
        if ports[0] == ports[1] || ports[0] == ports[2] || ports[1] == ports[2] || ports.contains(&listen_port) {
            bail!("{}", tr!("網頁、go2rtc API、RTSP、WebRTC 連接埠不可重複", "Web, go2rtc API, RTSP and WebRTC ports must all be different"));
        }
        if self.notify.schedule_enabled {
            let ok = |s: &str| {
                s.split_once(':').is_some_and(|(h, m)| {
                    h.parse::<u32>().is_ok_and(|h| h < 24) && m.len() == 2 && m.parse::<u32>().is_ok_and(|m| m < 60)
                })
            };
            if !ok(&self.notify.schedule_start) || !ok(&self.notify.schedule_end) {
                bail!("{}", tr!("通知時段格式錯誤，例如 22:00", "Invalid notification schedule time, e.g. 22:00"));
            }
            if self.notify.schedule_days.is_empty() || self.notify.schedule_days.iter().any(|d| *d > 6) {
                bail!("{}", tr!("通知時段至少要選一天", "Select at least one day for the notification schedule"));
            }
        }
        if self.notify.max_events < 10 {
            bail!("{}", tr!("事件保留數量至少 10", "Keep at least 10 events"));
        }
        let http_ok = |s: &str| s.starts_with("http://") || s.starts_with("https://");
        if !self.notify.ntfy_topic.is_empty() {
            if !http_ok(&self.notify.ntfy_url) {
                bail!("{}", tr!("ntfy 伺服器網址需以 http:// 或 https:// 開頭", "ntfy server URL must start with http:// or https://"));
            }
            if !self.notify.ntfy_topic.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                bail!("{}", tr!("ntfy 主題只能包含英數字、- 或 _", "ntfy topic may only contain letters, digits, - or _"));
            }
        }
        if !self.notify.public_url.is_empty() && !http_ok(&self.notify.public_url) {
            bail!("{}", tr!("對外網址需以 http:// 或 https:// 開頭", "Public URL must start with http:// or https://"));
        }
        if let Some(root) = self.recording.root() {
            if !root.is_absolute() {
                bail!("{}", tr!("錄影資料夾必須是完整路徑，例如 D:\\Tapo NVR\\recordings", "Recording folder must be a full path, e.g. D:\\Tapo NVR\\recordings"));
            }
        }
        if !(10..=3600).contains(&self.recording.segment_seconds) {
            bail!("{}", tr!("影片檔長度需介於 10–3600 秒", "Segment length must be between 10 and 3600 seconds"));
        }
        if self.recording.pre_seconds > 600 || self.recording.post_seconds > 3600 {
            bail!("{}", tr!("事件錄影前後秒數過大", "Event pre/post seconds are too large"));
        }
        let mut seen = std::collections::HashSet::new();
        for cam in &self.cameras {
            if !valid_camera_id(&cam.id) {
                bail!("{}", tr!("攝影機代號 `{}` 只能包含小寫英數字、- 或 _（最多 32 字）", "Camera ID `{}` may only contain lowercase letters, digits, - or _ (max 32)", cam.id));
            }
            if !seen.insert(cam.id.clone()) {
                bail!("{}", tr!("攝影機代號 `{}` 重複", "Duplicate camera ID `{}`", cam.id));
            }
            if cam.name.trim().is_empty() {
                bail!("{}", tr!("攝影機 `{}` 的名稱不可空白", "Camera `{}` needs a name", cam.id));
            }
            if cam.host.trim().is_empty() || cam.host.contains(['/', '@', ' ']) {
                bail!("{}", tr!("攝影機 `{}` 的 IP / 主機名稱格式錯誤", "Camera `{}` has an invalid IP / host name", cam.id));
            }
            if cam.stream == StreamSource::Tapo && cam.cloud_password.is_empty() {
                bail!("{}", tr!("攝影機 `{}` 使用 Tapo 私有串流，需要填入 TP-Link 雲端密碼", "Camera `{}` uses the Tapo stream and needs the TP-Link cloud password", cam.id));
            }
            if cam.rtsp_port == 0 || cam.onvif_port == 0 {
                bail!("{}", tr!("攝影機 `{}` 的連接埠不可為 0", "Camera `{}` ports cannot be 0", cam.id));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::NotifyConfig;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::Local> {
        chrono::Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn schedule() {
        // 2026-11-02 is a Monday
        let mut n = NotifyConfig { schedule_enabled: true, ..Default::default() };
        n.schedule_start = "22:00".into();
        n.schedule_end = "07:00".into();
        n.schedule_days = vec![1]; // Monday only
        assert!(n.in_schedule(at(2026, 11, 2, 23, 0))); // Mon night
        assert!(n.in_schedule(at(2026, 11, 3, 6, 59))); // Tue early morning belongs to Monday
        assert!(!n.in_schedule(at(2026, 11, 3, 7, 0)));
        assert!(!n.in_schedule(at(2026, 11, 2, 6, 0))); // Mon early morning belongs to Sunday
        assert!(!n.in_schedule(at(2026, 11, 3, 23, 0))); // Tue night
        n.schedule_start = "09:00".into();
        n.schedule_end = "18:00".into();
        assert!(n.in_schedule(at(2026, 11, 2, 12, 0)));
        assert!(!n.in_schedule(at(2026, 11, 2, 18, 0)));
        n.schedule_enabled = false;
        assert!(n.in_schedule(at(2026, 11, 3, 3, 0)));
    }
}