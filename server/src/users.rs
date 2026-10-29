//! 使用者管理（僅管理員）：新增家人等其他帳號，並指定「管理員」或「僅觀看」權限。
//! 主要管理員（首次設定建立的帳號）不能在這裡刪除或降級。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;

use crate::api::ApiError;
use crate::auth::CurrentUser;
use crate::config::{Role, UserAccount};
use crate::AppState;

type ApiResult = Result<Response, ApiError>;

pub async fn list(State(state): State<AppState>) -> Json<serde_json::Value> {
    let a = &state.cfg().auth;
    let mut users = vec![json!({"username": a.username, "role": Role::Admin, "primary": true})];
    users.extend(a.users.iter().map(|u| json!({"username": u.username, "role": u.role, "primary": false})));
    Json(json!(users))
}

#[derive(Deserialize)]
pub struct CreateReq {
    username: String,
    password: String,
    #[serde(default)]
    role: Role,
}

async fn hash(pw: String) -> Result<String, ApiError> {
    if pw.chars().count() < 8 {
        return Err(ApiError::bad(tr!("密碼至少 8 個字元", "Password must be at least 8 characters")));
    }
    tokio::task::spawn_blocking(move || crate::auth::hash_password(&pw))
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?
        .map_err(|e| ApiError::bad(e.to_string()))
}

pub async fn create(State(state): State<AppState>, Json(r): Json<CreateReq>) -> ApiResult {
    let username = r.username.trim().to_string();
    if username.is_empty() || username.chars().count() > 32 {
        return Err(ApiError::bad(tr!("帳號需為 1–32 字", "Username must be 1–32 characters")));
    }
    let password_hash = hash(r.password).await?;
    let name = username.clone();
    state
        .update_config(move |c| {
            if c.auth.find_user(&name).is_some() {
                anyhow::bail!("{}", tr!("帳號 `{name}` 已存在", "Username `{name}` already exists"));
            }
            c.auth.users.push(UserAccount { username: name, password_hash, role: r.role });
            Ok(())
        })
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?;
    tracing::info!("新增使用者 {username}（{:?}）", r.role);
    Ok(Json(json!({"ok": true})).into_response())
}

#[derive(Deserialize)]
pub struct UpdateReq {
    #[serde(default)]
    role: Option<Role>,
    /// 管理員幫忙重設密碼；空白 = 不變
    #[serde(default)]
    password: String,
}

pub async fn update(State(state): State<AppState>, Path(name): Path<String>, Json(r): Json<UpdateReq>) -> ApiResult {
    if name == state.cfg().auth.username {
        return Err(ApiError::bad(tr!("主要管理員請到「帳號安全」修改", "Change the primary administrator under Account security")));
    }
    let new_hash = if r.password.is_empty() { None } else { Some(hash(r.password).await?) };
    let reset = new_hash.is_some();
    let n = name.clone();
    state
        .update_config(move |c| {
            let Some(u) = c.auth.users.iter_mut().find(|u| u.username == n) else {
                anyhow::bail!("{}", tr!("找不到使用者 `{n}`", "User `{n}` not found"));
            };
            if let Some(role) = r.role {
                u.role = role;
            }
            if let Some(h) = new_hash {
                u.password_hash = h;
            }
            Ok(())
        })
        .await
        .map_err(|e| ApiError::bad(e.to_string()))?;
    if reset {
        // 密碼被重設：登出這位使用者的所有裝置
        state.auth.revoke_user(&name).await;
    }
    tracing::info!("更新使用者 {name}");
    Ok(Json(json!({"ok": true})).into_response())
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(me): Extension<CurrentUser>,
    Path(name): Path<String>,
) -> ApiResult {
    if name == state.cfg().auth.username {
        return Err(ApiError::bad(tr!("無法刪除主要管理員", "The primary administrator cannot be deleted")));
    }
    if name == me.username {
        return Err(ApiError::bad(tr!("無法刪除自己的帳號", "You cannot delete your own account")));
    }
    let n = name.clone();
    state
        .update_config(move |c| {
            let before = c.auth.users.len();
            c.auth.users.retain(|u| u.username != n);
            if c.auth.users.len() == before {
                anyhow::bail!("{}", tr!("找不到使用者 `{n}`", "User `{n}` not found"));
            }
            Ok(())
        })
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e.to_string()))?;
    state.auth.revoke_user(&name).await;
    tracing::info!("刪除使用者 {name}");
    Ok(Json(json!({"ok": true})).into_response())
}
