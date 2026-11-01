//! Admin plane: separate login, own session cookie, full visibility.

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use crate::dto::fifty;
use crate::error::{ApiError, ApiResult};
use crate::auth::{self, Admin};
use crate::db::{audit_kind, now, AUDIT_ADMIN, AUDIT_AGENT, AUDIT_SKILL};
use crate::market;
use crate::state::Shared;

/// Bootstrap the first admin from `ADMIN_USER` / `ADMIN_PASSWORD`.
/// Returns the generated password when none was supplied, so main can print it.
pub fn ensure_admin(app: &crate::state::App) -> Option<String> {
    let db = app.db();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM admin_users", [], |row| row.get(0))
        .unwrap_or(0);
    if count > 0 {
        return None;
    }
    let (password, generated) = if app.cfg.admin_password.is_empty() {
        let mut bytes = [0u8; 12];
        use rand::Rng;
        rand::thread_rng().fill(&mut bytes);
        (base64ish(&bytes), true)
    } else {
        (app.cfg.admin_password.clone(), false)
    };
    let hash = match auth::hash_password(&password) {
        Ok(hash) => hash,
        Err(err) => {
            error!(error = %err, "cannot hash admin password");
            return None;
        }
    };
    if let Err(err) = db.execute(
        "INSERT INTO admin_users (username, password_hash, created_at) VALUES (?1, ?2, ?3)",
        params![app.cfg.admin_user, hash, now()],
    ) {
        error!(error = %err, "cannot create admin user");
        return None;
    }
    if generated {
        Some(password)
    } else {
        None
    }
}

fn base64ish(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

impl FromRequestParts<Shared> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Shared) -> Result<Self, Self::Rejection> {
        let cookie = parts
            .headers
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok());
        let token = auth::session_token(cookie).ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "admin_login_required",
                "缺少管理员会话 Cookie，请重新登录",
            )
        })?;
        let username = app.sessions().username_for(&token).ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "admin_session_expired",
                "管理员会话无效或已过期，请重新登录",
            )
        })?;
        Ok(Admin(username))
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Bounds the work an unauthenticated caller can ask for: Argon2 over a
/// megabyte-long "password" is a cheap way to burn the server's CPU.
const MAX_USERNAME_LEN: usize = 64;
const MAX_PASSWORD_LEN: usize = 256;

/// POST /admin/api/login
pub async fn login(
    State(app): State<Shared>,
    Json(body): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    if body.username.len() > MAX_USERNAME_LEN || body.password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::bad_request(
            "invalid_credentials",
            "用户名或密码过长",
        ));
    }

    // Spent budget is checked before the database is touched, so a flood costs
    // neither a hash nor an audit row.
    let key = body.username.trim().to_lowercase();
    if let Some(wait) = app.logins().blocked_for(&key) {
        warn!(username = %body.username, wait, "admin login refused: too many failures");
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            format!("登录失败次数过多，请 {wait} 秒后再试"),
        ));
    }

    let hash: Option<String> = {
        let db = app.db();
        db.query_row(
            "SELECT password_hash FROM admin_users WHERE username = ?1",
            params![body.username],
            |row| row.get(0),
        )
        .ok()
    };
    let ok = hash
        .as_deref()
        .map(|stored| auth::verify_password(&body.password, stored))
        .unwrap_or(false);
    if !ok {
        // Do not reveal whether the username exists. The limiter caps how many
        // of these rows an anonymous caller can create.
        let tripped = app.logins().record_failure(&key);
        let db = app.db();
        let _ = audit_kind(
            &db,
            AUDIT_ADMIN,
            "anonymous",
            "admin_login_failed",
            None,
            None,
            Some(&body.username),
        );
        if tripped {
            let _ = audit_kind(
                &db,
                AUDIT_ADMIN,
                "anonymous",
                "admin_login_locked",
                None,
                None,
                Some(&key),
            );
        }
        warn!(username = %body.username, locked = tripped, "admin login failed");
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "用户名或密码错误",
        ));
    }
    app.logins().succeeded(&key);
    let token = app.sessions().issue(&body.username);
    {
        let db = app.db();
        let _ = audit_kind(
            &db,
            AUDIT_ADMIN,
            &format!("admin:{}", body.username),
            "admin_login",
            None,
            None,
            None,
        );
    }
    info!(username = %body.username, "admin login");
    let response = Json(json!({ "username": body.username })).into_response();
    let mut response = response;
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&auth::set_cookie(&token))
            .unwrap_or_else(|_| HeaderValue::from_static("market_admin=")),
    );
    Ok(response)
}

/// POST /admin/api/logout
pub async fn logout(State(app): State<Shared>, admin: Admin, parts: Parts) -> Response {
    let cookie = parts
        .headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok());
    if let Some(token) = auth::session_token(cookie) {
        app.sessions().revoke(&token);
    }
    let db = app.db();
    let _ = audit_kind(&db, AUDIT_ADMIN, &admin.actor(), "admin_logout", None, None, None);
    info!(admin = %admin.0, "admin logout");
    let mut response = Json(json!({ "ok": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("market_admin=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    response
}

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "fifty")]
    pub limit: i64,
}

/// GET /admin/api/skills — everything, including withdrawn and taken down.
pub async fn list_skills(
    State(app): State<Shared>,
    _admin: Admin,
    Query(query): Query<AdminListQuery>,
) -> Result<Json<Value>, ApiError> {
    let db = app.db();
    market::list_admin(
        &db,
        &market::SKILL,
        &query.q,
        query.limit.clamp(1, 200),
        &app.cfg.public_url,
    )
    .map(Json)
}

#[derive(Debug, Deserialize)]
pub struct TakedownRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// POST /admin/api/skills/{slug}/takedown — whole package, no self-restore.
pub async fn takedown(
    State(app): State<Shared>,
    admin: Admin,
    Path(slug): Path<String>,
    body: Option<Json<TakedownRequest>>,
) -> ApiResult<Json<Value>> {
    let reason = body
        .and_then(|Json(b)| b.reason)
        .filter(|r| !r.trim().is_empty());
    let db = app.db();
    let change = market::Change::Takedown { reason };
    market::change_state(&db, &market::SKILL, &slug, change, &admin.actor()).map(Json)
}

/// POST /admin/api/skills/{slug}/restore
pub async fn restore(
    State(app): State<Shared>,
    admin: Admin,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let db = app.db();
    market::change_state(&db, &market::SKILL, &slug, market::Change::Lift, &admin.actor()).map(Json)
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default = "two_hundred")]
    pub limit: i64,
    /// Optional `skill` / `agent` filter. Omitted (or unknown) means all.
    #[serde(default)]
    pub kind: Option<String>,
}

fn two_hundred() -> i64 {
    200
}

/// GET /admin/api/audit
pub async fn audit_log(
    State(app): State<Shared>,
    _admin: Admin,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Value>, ApiError> {
    let db = app.db();
    let kind = query
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| [AUDIT_SKILL, AUDIT_AGENT, AUDIT_ADMIN].contains(kind))
        .map(str::to_string);
    let mut stmt = db
        .prepare(
            "SELECT id, at, kind, actor, action, slug, version, detail
             FROM audit_log
             WHERE (?2 IS NULL OR kind = ?2)
             ORDER BY id DESC LIMIT ?1",
        )
        .map_err(ApiError::db)?;
    let items = stmt
        .query_map(params![query.limit.clamp(1, 1000), kind], |row| {
            Ok(json!({
                "id": row.get::<_, i64>(0)?,
                "at": row.get::<_, i64>(1)?,
                "kind": row.get::<_, String>(2)?,
                "actor": row.get::<_, String>(3)?,
                "action": row.get::<_, String>(4)?,
                "slug": row.get::<_, Option<String>>(5)?,
                "version": row.get::<_, Option<String>>(6)?,
                "detail": row.get::<_, Option<String>>(7)?,
            }))
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;
    debug!(items = items.len(), "audit log");
    Ok(Json(json!({ "items": items })))
}
