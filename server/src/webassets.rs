//! 內建網頁介面：release 版打包進執行檔；debug 版直接讀取 web/ 資料夾方便開發。

use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

#[derive(rust_embed::RustEmbed)]
#[folder = "../web/"]
struct Assets;

/// 簡體中文字典：取 i18n.js 內的繁體字典鍵值，由 zhconv 自動轉成簡體（只需維護繁體與英文）
pub async fn zh_cn() -> Response {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let body = CACHE.get_or_init(|| {
        let js = Assets::get("i18n.js").map(|f| String::from_utf8_lossy(&f.data).into_owned()).unwrap_or_default();
        let dict = js
            .split_once("/*DICT*/")
            .and_then(|(_, rest)| rest.split_once("/*END*/"))
            .and_then(|(json, _)| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json).ok())
            .unwrap_or_default();
        let out: serde_json::Map<String, serde_json::Value> = dict
            .keys()
            .map(|k| (k.clone(), serde_json::Value::String(crate::i18n::to_simplified(k))))
            .collect();
        serde_json::Value::Object(out).to_string()
    });
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")],
        body.clone(),
    )
        .into_response()
}

pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let if_none_match = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut path = uri.path().trim_start_matches('/').to_string();
    if path.is_empty() || path.ends_with('/') {
        path.push_str("index.html");
    }
    match Assets::get(&path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            // 以內容雜湊當 ETag：程式更新後瀏覽器一定拿到新版，未變更時回 304 省流量
            let etag = format!("\"{}\"", file.metadata.sha256_hash().iter().take(8).map(|b| format!("{b:02x}")).collect::<String>());
            if if_none_match.as_deref() == Some(etag.as_str()) {
                return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
            }
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                    (header::ETAG, etag),
                ],
                file.data,
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
