use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::store::Store;
use crate::AppState;

pub const COOKIE_NAME: &str = "tapo_session";
const MAX_FAILURES: u32 = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(600);

pub struct Auth {
    store: Arc<Store>,
    tickets: Mutex<HashMap<String, (String, Instant)>>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

#[derive(Clone)]
pub struct CurrentUser {
    pub username: String,
    pub token: String,
    pub role: crate::config::Role,
}

impl CurrentUser {
    pub fn is_admin(&self) -> bool {
        self.role == crate::config::Role::Admin
    }
}

impl Auth {
    /// Session 存在資料庫，只保存權杖的 SHA-256
    pub fn new(store: Arc<Store>) -> Self {
        Self { store, tickets: Mutex::new(HashMap::new()), failures: Mutex::new(HashMap::new()) }
    }

    async fn create(&self, username: &str, days: i64) -> (String, DateTime<Utc>) {
        let token = random_hex(32);
        let expires = Utc::now() + chrono::Duration::days(days);
        if let Err(e) = self.store.put_session(&token, username, expires) {
            tracing::error!("無法儲存 Session：{e}");
        }
        (token, expires)
    }

    pub async fn validate(&self, token: &str) -> Option<String> {
        self.store.session_user(token).ok().flatten()
    }

    /// 本機桌面版自動登入：建立新 Session 並回傳 60 秒內有效的一次性票證
    pub async fn issue_ticket(&self, username: &str, days: i64) -> String {
        let (token, _) = self.create(username, days).await;
        let ticket = random_hex(24);
        let mut t = self.tickets.lock().await;
        t.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(60));
        t.insert(ticket.clone(), (token, Instant::now()));
        ticket
    }

    /// 變更密碼後讓這位使用者的其他裝置全部登出，只保留目前這個 Session
    pub async fn revoke_others(&self, username: &str, keep: &str) {
        let _ = self.store.delete_user_sessions_except(username, keep);
        self.tickets.lock().await.clear();
    }

    /// 登出某位使用者的所有裝置
    pub async fn revoke_user(&self, username: &str) {
        let _ = self.store.delete_user_sessions(username);
        self.tickets.lock().await.clear();
    }

    pub async fn rename_user(&self, old: &str, new: &str) {
        let _ = self.store.rename_sessions(old, new);
    }

    async fn revoke(&self, token: &str) {
        let _ = self.store.delete_session(token);
    }
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("rng");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut raw = [0u8; 16];
    getrandom::getrandom(&mut raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    let salt = SaltString::encode_b64(&raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub fn client_ip(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> IpAddr {
    if state.cfg().server.trust_proxy_headers {
        let from_header = headers
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
            .or_else(|| {
                headers
                    .get("x-forwarded-for")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.split(',').next())
            })
            .and_then(|v| v.trim().parse().ok());
        if let Some(ip) = from_header {
            return ip;
        }
    }
    addr.ip()
}

fn is_https(state: &AppState, headers: &HeaderMap) -> bool {
    state.cfg().server.trust_proxy_headers
        && headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("https"))
}

/// 登入用的工作階段 Cookie：不設 Max-Age，關閉瀏覽器（或桌面版）後就會消失，需重新登入。
/// 伺服器端另有 session_days 上限，且伺服器每次啟動都會清除所有 Session。
fn session_cookie(state: &AppState, headers: &HeaderMap, token: &str) -> String {
    let secure = if is_https(state, headers) { "; Secure" } else { "" };
    format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax{secure}")
}

fn clear_cookie(state: &AppState, headers: &HeaderMap) -> String {
    let secure = if is_https(state, headers) { "; Secure" } else { "" };
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}")
}

fn token_from(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return Some(t.trim().to_string());
        }
    }
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE_NAME)
        .map(|(_, v)| v.to_string())
}

/// 狀態變更的請求必須同源，防止 CSRF
fn origin_ok(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    origin
        .split_once("://")
        .map(|(_, rest)| rest.trim_end_matches('/') == host)
        .unwrap_or(false)
}

pub async fn require_auth(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    if !state.cfg().auth.is_configured() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": tr!("尚未完成首次設定", "Initial setup is not complete"), "setup": true})),
        )
            .into_response();
    }
    let headers = req.headers();
    if req.method() != Method::GET && req.method() != Method::HEAD && !origin_ok(headers) {
        return (StatusCode::FORBIDDEN, Json(json!({"error": tr!("來源不符", "Origin mismatch")}))).into_response();
    }
    let Some(token) = token_from(headers) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": tr!("未登入", "Not signed in")}))).into_response();
    };
    match state.auth.validate(&token).await {
        Some(username) => {
            // 帳號被刪除後 Session 立即失效；角色每次從目前設定讀取，改權限立刻生效
            let Some(role) = state.cfg().auth.role_of(&username) else {
                return (StatusCode::UNAUTHORIZED, Json(json!({"error": tr!("登入已過期", "Session expired")}))).into_response();
            };
            req.extensions_mut().insert(CurrentUser { username, token, role });
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, Json(json!({"error": tr!("登入已過期", "Session expired")}))).into_response(),
    }
}

/// 只允許管理員（設定、攝影機控制、使用者管理等）；放在 require_auth 之後
pub async fn require_admin(req: Request, next: Next) -> Response {
    match req.extensions().get::<CurrentUser>() {
        Some(u) if u.is_admin() => next.run(req).await,
        _ => (StatusCode::FORBIDDEN, Json(json!({"error": tr!("需要管理員權限", "Administrator permission required"), "admin_required": true})))
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct LoginReq {
    username: String,
    password: String,
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginReq>,
) -> Response {
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, Json(json!({"error": tr!("來源不符", "Origin mismatch")}))).into_response();
    }
    if !state.cfg().auth.is_configured() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": tr!("尚未完成首次設定", "Initial setup is not complete"), "setup": true})),
        )
            .into_response();
    }
    let ip = client_ip(&state, &headers, addr);
    {
        let mut f = state.auth.failures.lock().await;
        f.retain(|_, (_, t)| t.elapsed() < FAILURE_WINDOW);
        if f.get(&ip).is_some_and(|(n, _)| *n >= MAX_FAILURES) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": tr!("嘗試次數過多，請 10 分鐘後再試", "Too many attempts, try again in 10 minutes")})),
            )
                .into_response();
        }
    }

    // 主要管理員與其他使用者都可登入；帳號不存在時仍做一次雜湊比對，避免從回應時間猜出帳號
    let hash = state
        .cfg()
        .auth
        .find_user(&body.username)
        .map(|(_, h)| h.to_string())
        .unwrap_or_else(|| state.cfg().auth.password_hash.clone());
    let known = state.cfg().auth.find_user(&body.username).is_some();
    let password = body.password.clone();
    let pw_ok = tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .unwrap_or(false);
    let ok = pw_ok && known;

    if !ok {
        let mut f = state.auth.failures.lock().await;
        let e = f.entry(ip).or_insert((0, Instant::now()));
        e.0 += 1;
        tracing::warn!("登入失敗：{ip} 使用者 `{}`", body.username);
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": tr!("帳號或密碼錯誤", "Wrong username or password")}))).into_response();
    }
    state.auth.failures.lock().await.remove(&ip);

    let days = state.cfg().auth.session_days;
    let (token, expires) = state.auth.create(&body.username, days).await;
    tracing::info!("登入成功：{ip}");
    (
        [(header::SET_COOKIE, session_cookie(&state, &headers, &token))],
        Json(json!({"token": token, "username": body.username, "expires": expires})),
    )
        .into_response()
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Extension(user): axum::Extension<CurrentUser>,
) -> Response {
    state.auth.revoke(&user.token).await;
    (
        [(header::SET_COOKIE, clear_cookie(&state, &headers))],
        Json(json!({"ok": true})),
    )
        .into_response()
}

pub async fn me(State(state): State<AppState>, axum::Extension(user): axum::Extension<CurrentUser>) -> Json<serde_json::Value> {
    let primary = user.username == state.cfg().auth.username;
    Json(json!({"username": user.username, "role": user.role, "admin": user.is_admin(), "primary": primary}))
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    current_password: String,
    new_password: String,
}

/// 任何使用者都可以修改自己的密碼（變更後登出自己的其他裝置）
pub async fn change_own_password(
    State(state): State<AppState>,
    axum::Extension(user): axum::Extension<CurrentUser>,
    Json(r): Json<ChangePasswordReq>,
) -> Response {
    let err = |code: StatusCode, msg: String| (code, Json(json!({"error": msg}))).into_response();
    let Some((_, hash)) = state.cfg().auth.find_user(&user.username).map(|(r, h)| (r, h.to_string())) else {
        return err(StatusCode::UNAUTHORIZED, tr!("登入已過期", "Session expired"));
    };
    let current = r.current_password.clone();
    if !tokio::task::spawn_blocking(move || verify_password(&current, &hash)).await.unwrap_or(false) {
        return err(StatusCode::FORBIDDEN, tr!("目前密碼錯誤", "Current password is wrong"));
    }
    if r.new_password.chars().count() < 8 {
        return err(StatusCode::BAD_REQUEST, tr!("新密碼至少 8 個字元", "New password must be at least 8 characters"));
    }
    let pw = r.new_password.clone();
    let new_hash = match tokio::task::spawn_blocking(move || hash_password(&pw)).await {
        Ok(Ok(h)) => h,
        _ => return err(StatusCode::INTERNAL_SERVER_ERROR, tr!("無法產生密碼雜湊", "Cannot hash password")),
    };
    let name = user.username.clone();
    let result = state
        .update_config(move |c| {
            if c.auth.username == name {
                c.auth.password_hash = new_hash;
            } else if let Some(u) = c.auth.users.iter_mut().find(|u| u.username == name) {
                u.password_hash = new_hash;
            }
            Ok(())
        })
        .await;
    if let Err(e) = result {
        return err(StatusCode::BAD_REQUEST, e.to_string());
    }
    state.auth.revoke_others(&user.username, &user.token).await;
    tracing::info!("使用者 {} 已變更密碼", user.username);
    Json(json!({"ok": true})).into_response()
}

/// 桌面版：用 Bearer token 換一次性票證，讓 WebView 以 /auth/consume 取得 Cookie
pub async fn create_ticket(
    State(state): State<AppState>,
    axum::Extension(user): axum::Extension<CurrentUser>,
) -> Json<serde_json::Value> {
    let ticket = random_hex(24);
    let mut t = state.auth.tickets.lock().await;
    t.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(60));
    t.insert(ticket.clone(), (user.token, Instant::now()));
    Json(json!({"ticket": ticket}))
}

#[derive(Deserialize)]
pub struct ConsumeQuery {
    ticket: String,
    #[serde(default)]
    client: Option<String>,
}

pub async fn consume_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ConsumeQuery>,
) -> Response {
    let entry = state.auth.tickets.lock().await.remove(&q.ticket);
    match entry {
        Some((token, at)) if at.elapsed() < Duration::from_secs(60) => {
            if state.auth.validate(&token).await.is_none() {
                return Redirect::to("/login.html").into_response();
            }
            (
                [(header::SET_COOKIE, session_cookie(&state, &headers, &token))],
                Redirect::to(if q.client.as_deref() == Some("desktop") { "/?client=desktop" } else { "/" }),
            )
                .into_response()
        }
        _ => Redirect::to("/login.html").into_response(),
    }
}

fn is_local_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback() || v4.is_private())
        }
    }
}

pub async fn setup_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({"needs_setup": !state.cfg().auth.is_configured()}))
}

#[derive(Deserialize)]
pub struct SetupReq {
    username: String,
    password: String,
}

/// 首次設定：建立管理員帳密。僅限尚未設定時、且只接受本機 / 區網直連（經代理轉送一律拒絕）
pub async fn setup(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<SetupReq>,
) -> Response {
    let err = |code: StatusCode, msg: &str| (code, Json(json!({"error": msg}))).into_response();
    if state.cfg().auth.is_configured() {
        return err(StatusCode::CONFLICT, &tr!("已完成設定，請直接登入", "Setup is already complete, please sign in"));
    }
    if !origin_ok(&headers) {
        return err(StatusCode::FORBIDDEN, &tr!("來源不符", "Origin mismatch"));
    }
    let proxied = ["cf-connecting-ip", "x-forwarded-for", "x-real-ip", "forwarded"]
        .iter()
        .any(|h| headers.contains_key(*h));
    if proxied || !is_local_network(addr.ip()) {
        return err(StatusCode::FORBIDDEN, &tr!("基於安全考量，首次設定只能在本機或區網內進行", "For security, initial setup must be done from this computer or the local network"));
    }
    let username = body.username.trim().to_string();
    if username.is_empty() || username.chars().count() > 32 {
        return err(StatusCode::BAD_REQUEST, &tr!("帳號需為 1–32 字", "Username must be 1–32 characters"));
    }
    if body.password.chars().count() < 8 {
        return err(StatusCode::BAD_REQUEST, &tr!("密碼至少 8 個字元", "Password must be at least 8 characters"));
    }
    let password = body.password.clone();
    let hash = match tokio::task::spawn_blocking(move || hash_password(&password)).await {
        Ok(Ok(h)) => h,
        _ => return err(StatusCode::INTERNAL_SERVER_ERROR, &tr!("無法產生密碼雜湊", "Cannot hash password")),
    };
    let u = username.clone();
    let result = state
        .update_config(move |c| {
            if c.auth.is_configured() {
                anyhow::bail!("{}", tr!("已完成設定，請直接登入", "Setup is already complete, please sign in"));
            }
            c.auth.username = u;
            c.auth.password_hash = hash;
            Ok(())
        })
        .await;
    if let Err(e) = result {
        return err(StatusCode::CONFLICT, &e.to_string());
    }
    tracing::info!("已完成首次設定（管理員：{username}）");
    let days = state.cfg().auth.session_days;
    let (token, expires) = state.auth.create(&username, days).await;
    (
        [(header::SET_COOKIE, session_cookie(&state, &headers, &token))],
        Json(json!({"token": token, "username": username, "expires": expires})),
    )
        .into_response()
}
