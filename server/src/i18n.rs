//! 多語系：繁體中文（原文）、簡體中文（由繁體自動轉換）、英文。
//!
//! - HTTP 請求：依 `X-Lang`（使用者在介面手動選擇）或 `Accept-Language`（瀏覽器 / 系統語言）
//! - 背景工作（手機推播等）：依系統語言，或設定檔 `server.language`
//!
//! 用法：`tr!("找不到攝影機 {id}", "Camera {id} not found")`

use std::sync::RwLock;

use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    ZhTw,
    ZhCn,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::ZhTw => "zh-TW",
            Lang::ZhCn => "zh-CN",
            Lang::En => "en",
        }
    }
}

tokio::task_local! {
    static CURRENT: Lang;
}

static DEFAULT: RwLock<Option<Lang>> = RwLock::new(None);

/// 解析語言標籤；不支援的語言回傳 None
pub fn parse(tag: &str) -> Option<Lang> {
    let t = tag.trim().to_ascii_lowercase().replace('_', "-");
    if t.starts_with("zh") {
        let simplified = ["-cn", "-sg", "-my", "-hans"].iter().any(|s| t.contains(s));
        Some(if simplified { Lang::ZhCn } else { Lang::ZhTw })
    } else if t.starts_with("en") {
        Some(Lang::En)
    } else {
        None
    }
}

/// 系統語言（Windows 顯示語言）；非中文一律使用英文
pub fn system() -> Lang {
    sys_locale::get_locales()
        .find_map(|l| parse(&l))
        .or_else(|| sys_locale::get_locale().map(|_| Lang::En))
        .unwrap_or(Lang::En)
}

/// 設定預設語言（"auto" = 系統語言）
pub fn set_default(setting: &str) {
    let lang = if setting.eq_ignore_ascii_case("auto") || setting.is_empty() {
        system()
    } else {
        parse(setting).unwrap_or_else(system)
    };
    *DEFAULT.write().unwrap() = Some(lang);
}

pub fn default_lang() -> Lang {
    if let Some(l) = *DEFAULT.read().unwrap() {
        return l;
    }
    let l = system();
    *DEFAULT.write().unwrap() = Some(l);
    l
}

pub fn current() -> Lang {
    CURRENT.try_with(|l| *l).unwrap_or_else(|_| default_lang())
}

pub fn from_headers(headers: &HeaderMap) -> Lang {
    if let Some(l) = headers.get("x-lang").and_then(|v| v.to_str().ok()).and_then(parse) {
        return l;
    }
    if let Some(al) = headers.get("accept-language").and_then(|v| v.to_str().ok()) {
        // 依瀏覽器的偏好順序，取第一個支援的語言
        let mut tags: Vec<(f32, &str)> = al
            .split(',')
            .map(|part| {
                let mut it = part.split(';');
                let tag = it.next().unwrap_or("").trim();
                let q = it
                    .find_map(|p| p.trim().strip_prefix("q="))
                    .and_then(|q| q.parse().ok())
                    .unwrap_or(1.0);
                (q, tag)
            })
            .collect();
        tags.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(l) = tags.iter().find_map(|(_, t)| parse(t)) {
            return l;
        }
        if !tags.is_empty() {
            return Lang::En;
        }
    }
    default_lang()
}

/// 讓整個請求處理過程使用該請求的語言
pub async fn layer(req: Request, next: Next) -> Response {
    let lang = from_headers(req.headers());
    CURRENT.scope(lang, next.run(req)).await
}

/// 在指定語言下執行（背景工作）
pub async fn with<F: std::future::Future>(lang: Lang, f: F) -> F::Output {
    CURRENT.scope(lang, f).await
}

pub fn to_simplified(s: &str) -> String {
    zhconv::zhconv(s, zhconv::Variant::ZhCN)
}

#[macro_export]
macro_rules! tr {
    ($zh:literal, $en:literal $(, $($arg:tt)*)?) => {
        match $crate::i18n::current() {
            $crate::i18n::Lang::En => format!($en $(, $($arg)*)?),
            $crate::i18n::Lang::ZhTw => format!($zh $(, $($arg)*)?),
            $crate::i18n::Lang::ZhCn => $crate::i18n::to_simplified(&format!($zh $(, $($arg)*)?)),
        }
    };
}
