//! 系統記錄：保留最近的記錄在記憶體中，供設定頁「系統記錄」檢視（不需要去找記錄檔）。
//! 獨立版與桌面版初始化 tracing 時都要加上 `tapo_nvr::log_layer()`。

use std::collections::VecDeque;
use std::fmt::Write;
use std::sync::Mutex;

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

const MAX_LINES: usize = 2000;

#[derive(Clone, Serialize)]
pub struct LogLine {
    pub ts: String,
    pub level: &'static str,
    pub target: String,
    pub message: String,
}

static BUF: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());

pub struct BufferLayer;

struct MsgVisitor(String);

impl Visit for MsgVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for BufferLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = *meta.level();
        if level > tracing::Level::INFO {
            return;
        }
        // 只收本程式與 go2rtc 的記錄，略過底層函式庫的雜訊
        let target = meta.target();
        if !(target.starts_with("tapo") || target == "go2rtc") {
            return;
        }
        let mut v = MsgVisitor(String::new());
        event.record(&mut v);
        let line = LogLine {
            ts: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            level: match level {
                tracing::Level::ERROR => "ERROR",
                tracing::Level::WARN => "WARN",
                _ => "INFO",
            },
            target: target.to_string(),
            message: v.0,
        };
        let mut buf = BUF.lock().unwrap_or_else(|e| e.into_inner());
        buf.push_back(line);
        while buf.len() > MAX_LINES {
            buf.pop_front();
        }
    }
}

pub fn layer() -> BufferLayer {
    BufferLayer
}

/// 最新的記錄（由舊到新）；level = "warn" 只回傳警告與錯誤
pub fn lines(limit: usize, warn_only: bool) -> Vec<LogLine> {
    let buf = BUF.lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<LogLine> = buf.iter().rev().filter(|l| !warn_only || l.level != "INFO").take(limit).cloned().collect();
    out.reverse();
    out
}
