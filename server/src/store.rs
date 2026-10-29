//! 設定與資料儲存（SQLite：data/tapo-nvr.db）
//!
//! - settings：各區段設定（JSON，不含機密）
//! - cameras：攝影機（JSON，不含機密），依 position 排序
//! - secrets：攝影機密碼、TP-Link 雲端密碼、ntfy 權杖，以 DPAPI 加密（見 secret.rs）
//! - sessions：登入 Session，只存權杖的 SHA-256，資料庫外洩也無法冒用
//! - events：移動偵測事件

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::config::{CameraConfig, Config};
use crate::events::Event;
use crate::secret;

pub const DB_FILE: &str = "tapo-nvr.db";

pub struct Store {
    conn: Mutex<Connection>,
    dir: PathBuf,
    pub path: PathBuf,
}

/// 資料庫中的時間一律存成固定格式的 UTC 字串，字串比較即等於時間比較
fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| tr!("無法建立資料夾", "Cannot create data folder"))?;
        let path = dir.join(DB_FILE);
        let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS cameras (id TEXT PRIMARY KEY, position INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS sessions (token_hash TEXT PRIMARY KEY, username TEXT NOT NULL, expires TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts TEXT NOT NULL, camera TEXT NOT NULL, camera_name TEXT NOT NULL,
                 kind TEXT NOT NULL, snapshot TEXT, notify INTEGER NOT NULL DEFAULT 1);
             CREATE INDEX IF NOT EXISTS events_camera_ts ON events (camera, ts);
             CREATE TABLE IF NOT EXISTS motion (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 camera TEXT NOT NULL, start TEXT NOT NULL, end TEXT);
             CREATE INDEX IF NOT EXISTS motion_camera_start ON motion (camera, start);
             INSERT OR IGNORE INTO meta (key, value) VALUES ('schema', '1');",
        )?;
        Ok(Self { conn: Mutex::new(conn), dir: dir.to_path_buf(), path })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------------
    // 設定

    pub fn has_config(&self) -> Result<bool> {
        Ok(self.conn().query_row("SELECT COUNT(*) FROM settings", [], |r| r.get::<_, i64>(0))? > 0)
    }

    pub fn load_config(&self) -> Result<Option<Config>> {
        if !self.has_config()? {
            return Ok(None);
        }
        let conn = self.conn();
        let section = |key: &str| -> Result<Option<String>> {
            Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
        };
        fn parse<T: DeserializeOwned + Default>(s: Option<String>) -> T {
            s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
        }
        let mut cfg = Config {
            server: parse(section("server")?),
            auth: parse(section("auth")?),
            go2rtc: parse(section("go2rtc")?),
            notify: parse(section("notify")?),
            recording: parse(section("recording")?),
            update: parse(section("update")?),
            cameras: Vec::new(),
        };
        let mut stmt = conn.prepare("SELECT data FROM cameras ORDER BY position, id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            match serde_json::from_str::<CameraConfig>(&row?) {
                Ok(c) => cfg.cameras.push(c),
                Err(e) => tracing::error!("無法讀取攝影機設定：{e}"),
            }
        }
        drop(stmt);
        let secret = |name: &str| -> Result<String> {
            let blob: Option<Vec<u8>> = conn.query_row("SELECT value FROM secrets WHERE name = ?1", [name], |r| r.get(0)).optional()?;
            Ok(match blob {
                Some(b) => String::from_utf8(secret::unprotect(&self.dir, &b)?).unwrap_or_default(),
                None => String::new(),
            })
        };
        cfg.notify.ntfy_token = secret("notify/ntfy_token")?;
        for cam in &mut cfg.cameras {
            cam.password = secret(&format!("camera/{}/password", cam.id))?;
            cam.cloud_password = secret(&format!("camera/{}/cloud_password", cam.id))?;
        }
        Ok(Some(cfg))
    }

    /// 以單一交易寫入全部設定；機密欄位另外加密儲存
    pub fn save_config(&self, cfg: &Config) -> Result<()> {
        let mut secrets: Vec<(String, String)> = Vec::new();
        let mut notify = cfg.notify.clone();
        secrets.push(("notify/ntfy_token".into(), std::mem::take(&mut notify.ntfy_token)));
        let mut cams = Vec::new();
        for (i, c) in cfg.cameras.iter().enumerate() {
            let mut c = c.clone();
            secrets.push((format!("camera/{}/password", c.id), std::mem::take(&mut c.password)));
            secrets.push((format!("camera/{}/cloud_password", c.id), std::mem::take(&mut c.cloud_password)));
            cams.push((c.id.clone(), i as i64, serde_json::to_string(&c)?));
        }
        let encrypted: Vec<(String, Option<Vec<u8>>)> = secrets
            .into_iter()
            .map(|(k, v)| Ok((k, if v.is_empty() { None } else { Some(secret::protect(&self.dir, v.as_bytes())?) })))
            .collect::<Result<_>>()?;

        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (key, value) in [
            ("server", serde_json::to_string(&cfg.server)?),
            ("auth", serde_json::to_string(&cfg.auth)?),
            ("go2rtc", serde_json::to_string(&cfg.go2rtc)?),
            ("notify", serde_json::to_string(&notify)?),
            ("recording", serde_json::to_string(&cfg.recording)?),
            ("update", serde_json::to_string(&cfg.update)?),
        ] {
            tx.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![key, value])?;
        }
        tx.execute("DELETE FROM cameras", [])?;
        for (id, pos, data) in &cams {
            tx.execute("INSERT INTO cameras (id, position, data) VALUES (?1, ?2, ?3)", params![id, pos, data])?;
        }
        tx.execute("DELETE FROM secrets", [])?;
        for (name, value) in &encrypted {
            if let Some(v) = value {
                tx.execute("INSERT INTO secrets (name, value) VALUES (?1, ?2)", params![name, v])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Session（只存權杖雜湊）

    pub fn session_user(&self, token: &str) -> Result<Option<String>> {
        let now = Utc::now().to_rfc3339();
        Ok(self
            .conn()
            .query_row(
                "SELECT username FROM sessions WHERE token_hash = ?1 AND expires > ?2",
                params![token_hash(token), now],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn put_session(&self, token: &str, username: &str, expires: DateTime<Utc>) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM sessions WHERE expires <= ?1", [Utc::now().to_rfc3339()])?;
        conn.execute(
            "INSERT OR REPLACE INTO sessions (token_hash, username, expires) VALUES (?1, ?2, ?3)",
            params![token_hash(token), username, expires.to_rfc3339()],
        )?;
        Ok(())
    }

    /// 伺服器啟動時呼叫：應用程式或伺服器完全關閉後重開，所有人都必須重新登入
    pub fn clear_sessions(&self) -> Result<()> {
        self.conn().execute("DELETE FROM sessions", [])?;
        Ok(())
    }

    pub fn delete_session(&self, token: &str) -> Result<()> {
        self.conn().execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash(token)])?;
        Ok(())
    }

    /// 某位使用者除了 keep_token 以外的 Session 全部刪除（變更密碼後登出其他裝置）
    pub fn delete_user_sessions_except(&self, username: &str, keep_token: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM sessions WHERE username = ?1 AND token_hash <> ?2",
            params![username, token_hash(keep_token)],
        )?;
        Ok(())
    }

    /// 刪除某位使用者的所有 Session（刪除帳號、管理員重設密碼時）
    pub fn delete_user_sessions(&self, username: &str) -> Result<()> {
        self.conn().execute("DELETE FROM sessions WHERE username = ?1", [username])?;
        Ok(())
    }

    pub fn rename_sessions(&self, old: &str, new: &str) -> Result<()> {
        self.conn().execute("UPDATE sessions SET username = ?2 WHERE username = ?1", params![old, new])?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 事件

    pub fn insert_event(&self, ev: &Event) -> Result<u64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (ts, camera, camera_name, kind, snapshot, notify) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ts(ev.ts), ev.camera, ev.camera_name, ev.kind, ev.snapshot, ev.notify],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    /// 最新的 limit 筆事件（由舊到新）
    pub fn recent_events(&self, limit: usize) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, camera, camera_name, kind, snapshot, notify FROM events ORDER BY id DESC LIMIT ?1",
        )?;
        let mut out: Vec<Event> = stmt
            .query_map([limit as i64], |r| {
                Ok(Event {
                    id: r.get::<_, i64>(0)? as u64,
                    ts: r
                        .get::<_, String>(1)?
                        .parse::<DateTime<Utc>>()
                        .unwrap_or_else(|_| Utc::now()),
                    camera: r.get(2)?,
                    camera_name: r.get(3)?,
                    kind: r.get(4)?,
                    snapshot: r.get(5)?,
                    notify: r.get(6)?,
                })
            })?
            .filter_map(|e| e.ok())
            .collect();
        out.reverse();
        Ok(out)
    }

    /// 某台攝影機在時間範圍內的事件（由舊到新）
    pub fn events_between(&self, camera: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, camera, camera_name, kind, snapshot, notify FROM events
             WHERE camera = ?1 AND ts >= ?2 AND ts <= ?3 ORDER BY ts",
        )?;
        let rows = stmt.query_map(params![camera, ts(from), ts(to)], |r| {
            Ok(Event {
                id: r.get::<_, i64>(0)? as u64,
                ts: r.get::<_, String>(1)?.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()),
                camera: r.get(2)?,
                camera_name: r.get(3)?,
                kind: r.get(4)?,
                snapshot: r.get(5)?,
                notify: r.get(6)?,
            })
        })?;
        Ok(rows.filter_map(|e| e.ok()).collect())
    }

    /// 最後一筆離線 / 恢復連線事件（重新啟動後沿用，避免重複通知仍在離線的攝影機）
    pub fn last_reach_event(&self, camera: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT kind FROM events WHERE camera = ?1 AND kind IN ('offline', 'online') ORDER BY id DESC LIMIT 1",
                [camera],
                |r| r.get(0),
            )
            .optional()?)
    }

    // ------------------------------------------------------------------
    // 移動區間（事件錄影據此決定保留哪些片段；存在資料庫，重新啟動後仍有效）

    pub fn motion_start(&self, camera: &str, at: DateTime<Utc>) -> Result<()> {
        let conn = self.conn();
        let open: Option<i64> = conn
            .query_row("SELECT id FROM motion WHERE camera = ?1 AND end IS NULL", [camera], |r| r.get(0))
            .optional()?;
        if open.is_none() {
            conn.execute("INSERT INTO motion (camera, start) VALUES (?1, ?2)", params![camera, ts(at)])?;
        }
        Ok(())
    }

    pub fn motion_end(&self, camera: &str, at: DateTime<Utc>) -> Result<()> {
        self.conn()
            .execute("UPDATE motion SET end = ?2 WHERE camera = ?1 AND end IS NULL", params![camera, ts(at)])?;
        Ok(())
    }

    /// 與時間範圍重疊的移動區間；尚未結束的以 now 為結束時間
    pub fn motion_between(
        &self,
        camera: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<(DateTime<Utc>, DateTime<Utc>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT start, end FROM motion WHERE camera = ?1 AND start <= ?3 AND (end IS NULL OR end >= ?2)",
        )?;
        let now = Utc::now();
        let rows = stmt.query_map(params![camera, ts(from), ts(to)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        Ok(rows
            .filter_map(|r| r.ok())
            .filter_map(|(s, e)| {
                let s = s.parse::<DateTime<Utc>>().ok()?;
                let e = e.and_then(|e| e.parse::<DateTime<Utc>>().ok()).unwrap_or(now);
                Some((s, e))
            })
            .collect())
    }

    /// 啟動時：上次未正常結束的區間以開始時間收尾；刪除 7 天前的區間
    pub fn motion_housekeeping(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute("UPDATE motion SET end = start WHERE end IS NULL", [])?;
        conn.execute("DELETE FROM motion WHERE start < ?1", [ts(Utc::now() - chrono::Duration::days(7))])?;
        Ok(())
    }

    /// 只保留最新 max 筆，回傳被刪除事件的截圖檔名
    pub fn prune_events(&self, max: usize) -> Result<Vec<String>> {
        let conn = self.conn();
        let cutoff: Option<i64> = conn
            .query_row("SELECT id FROM events ORDER BY id DESC LIMIT 1 OFFSET ?1", [max as i64], |r| r.get(0))
            .optional()?;
        let Some(cutoff) = cutoff else { return Ok(Vec::new()) };
        let mut stmt = conn.prepare("SELECT snapshot FROM events WHERE id <= ?1 AND snapshot IS NOT NULL")?;
        let snaps: Vec<String> = stmt.query_map([cutoff], |r| r.get(0))?.filter_map(|s| s.ok()).collect();
        drop(stmt);
        conn.execute("DELETE FROM events WHERE id <= ?1", [cutoff])?;
        Ok(snaps)
    }
}

/// 舊版移轉：匯入 config.toml / sessions.json / events.jsonl，並清除舊檔中的明碼機密
pub fn migrate_legacy(store: &Store, legacy_config: Option<&Path>, data_dir: &Path) -> Result<Option<Config>> {
    let mut imported = None;
    if let Some(path) = legacy_config.filter(|p| p.exists()) {
        if !store.has_config()? {
            let cfg = Config::from_toml_file(path)?;
            store.save_config(&cfg)?;
            tracing::info!("已從 {} 匯入設定到資料庫（{} 台攝影機）", path.display(), cfg.cameras.len());
            scrub_legacy_toml(path)?;
            imported = Some(cfg);
        }
    }
    let events = data_dir.join("events.jsonl");
    if events.exists() {
        let text = std::fs::read_to_string(&events).unwrap_or_default();
        let mut n = 0;
        for ev in text.lines().filter_map(|l| serde_json::from_str::<Event>(l).ok()) {
            let _ = store.insert_event(&ev);
            n += 1;
        }
        let _ = std::fs::rename(&events, data_dir.join("events.jsonl.imported"));
        tracing::info!("已匯入 {n} 筆事件");
    }
    // 舊的 sessions.json 存的是明碼權杖：直接刪除（使用者重新登入一次即可）
    let _ = std::fs::remove_file(data_dir.join("sessions.json"));
    let _ = std::fs::remove_file(data_dir.join("go2rtc.yaml"));
    Ok(imported)
}

/// 把舊設定檔改名為 .imported，並把其中的密碼 / 權杖清空
fn scrub_legacy_toml(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml::Value = toml::from_str(text.trim_start_matches('\u{feff}'))?;
    if let Some(cams) = doc.get_mut("cameras").and_then(|c| c.as_array_mut()) {
        for c in cams {
            if let Some(t) = c.as_table_mut() {
                for k in ["password", "cloud_password"] {
                    if t.contains_key(k) {
                        t.insert(k.into(), toml::Value::String(String::new()));
                    }
                }
            }
        }
    }
    if let Some(n) = doc.get_mut("notify").and_then(|n| n.as_table_mut()) {
        if n.contains_key("ntfy_token") {
            n.insert("ntfy_token".into(), toml::Value::String(String::new()));
        }
    }
    let backup = path.with_extension("toml.imported");
    std::fs::write(
        &backup,
        format!(
            "# 已匯入 Tapo NVR 資料庫（data/{DB_FILE}），密碼已從此檔移除；可安全刪除。\n# Imported into the Tapo NVR database; secrets were removed from this file.\n\n{}",
            toml::to_string_pretty(&doc)?
        ),
    )?;
    std::fs::remove_file(path)?;
    Ok(())
}
