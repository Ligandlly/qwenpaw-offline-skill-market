//! Admin plane: separate login, own session cookie, full visibility.

use argon2::password_hash::{rand_core::OsRng, SaltString};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use crate::api::{ApiError, SkillRow};
use crate::auth::{self, Admin};
use crate::db::{audit, now};
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
                "admin session cookie is missing",
            )
        })?;
        let username = app.sessions().username_for(&token).ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "admin_session_expired",
                "admin session is invalid or expired",
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

/// POST /admin/api/login
pub async fn login(
    State(app): State<Shared>,
    Json(body): Json<LoginRequest>,
) -> Result<Response, ApiError> {
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
        // Do not reveal whether the username exists.
        let db = app.db();
        let _ = audit(
            &db,
            "anonymous",
            "admin_login_failed",
            None,
            None,
            Some(&body.username),
        );
        warn!(username = %body.username, "admin login failed");
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "invalid username or password",
        ));
    }
    let token = app.sessions().issue(&body.username);
    {
        let db = app.db();
        let _ = audit(
            &db,
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
    let _ = audit(&db, &admin.actor(), "admin_logout", None, None, None);
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

fn fifty() -> i64 {
    50
}

/// GET /admin/api/skills — everything, including withdrawn and taken down.
pub async fn list_skills(
    State(app): State<Shared>,
    _admin: Admin,
    Query(query): Query<AdminListQuery>,
) -> Result<Json<Value>, ApiError> {
    let pattern = format!("%{}%", query.q.trim());
    let limit = query.limit.clamp(1, 200);
    let db = app.db();
    let sql = r#"
        SELECT s.slug, s.name, s.owner_tenant, t.display_name AS owner_name,
               s.visibility, s.latest_version,
               s.created_at, s.withdrawn_at, s.takedown_at, s.takedown_reason,
               (SELECT json_extract(v.bundle_json, '$."SKILL.md"')
                  FROM skill_versions v
                 WHERE v.slug = s.slug AND v.version = s.latest_version) AS manifest,
               COALESCE((SELECT COUNT(*) FROM skill_versions v WHERE v.slug = s.slug), 0) AS versions,
               COALESCE((SELECT SUM(size_bytes) FROM skill_versions v WHERE v.slug = s.slug), 0) AS size_bytes
        FROM skills s
        LEFT JOIN tenants t ON t.id = s.owner_tenant
        WHERE s.slug LIKE ?1 OR s.name LIKE ?1
        ORDER BY s.created_at DESC
        LIMIT ?2
    "#;
    let mut stmt = db.prepare(sql).map_err(internal)?;
    let items = stmt
        .query_map(params![pattern, limit], |row| {
            row_to_admin_skill(row, &app.cfg.public_url)
        })
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(internal)?;
    debug!(items = items.len(), "admin skill listing");
    Ok(Json(json!({ "items": items })))
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
) -> Result<Json<Value>, ApiError> {
    let reason = body
        .and_then(|Json(b)| b.reason)
        .filter(|r| !r.trim().is_empty());
    let db = app.db();
    let changed = db
        .execute(
            "UPDATE skills SET takedown_at = ?2, takedown_by = ?3, takedown_reason = ?4
             WHERE slug = ?1",
            params![slug, now(), admin.0, reason],
        )
        .map_err(internal)?;
    if changed == 0 {
        warn!(admin = %admin.0, slug = %slug, "takedown: unknown skill");
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "unknown skill",
        ));
    }
    info!(
        admin = %admin.0,
        slug = %slug,
        reason = reason.as_deref().unwrap_or("-"),
        "skill taken down"
    );
    audit(
        &db,
        &admin.actor(),
        "takedown",
        Some(&slug),
        None,
        reason.as_deref(),
    )
    .map_err(internal)?;
    Ok(Json(json!({ "slug": slug, "state": "taken_down" })))
}

/// POST /admin/api/skills/{slug}/restore
pub async fn restore(
    State(app): State<Shared>,
    admin: Admin,
    Path(slug): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let db = app.db();
    let changed = db
        .execute(
            "UPDATE skills SET takedown_at = NULL, takedown_by = NULL, takedown_reason = NULL
             WHERE slug = ?1",
            params![slug],
        )
        .map_err(internal)?;
    if changed == 0 {
        warn!(admin = %admin.0, slug = %slug, "admin restore: unknown skill");
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "unknown skill",
        ));
    }
    info!(admin = %admin.0, slug = %slug, "takedown revoked");
    audit(
        &db,
        &admin.actor(),
        "admin_restore",
        Some(&slug),
        None,
        None,
    )
    .map_err(internal)?;
    Ok(Json(json!({ "slug": slug, "state": "active" })))
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default = "two_hundred")]
    pub limit: i64,
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
    let mut stmt = db
        .prepare(
            "SELECT id, at, actor, action, slug, version, detail
             FROM audit_log ORDER BY id DESC LIMIT ?1",
        )
        .map_err(internal)?;
    let items = stmt
        .query_map(params![query.limit.clamp(1, 1000)], |row| {
            Ok(json!({
                "id": row.get::<_, i64>(0)?,
                "at": row.get::<_, i64>(1)?,
                "actor": row.get::<_, String>(2)?,
                "action": row.get::<_, String>(3)?,
                "slug": row.get::<_, Option<String>>(4)?,
                "version": row.get::<_, Option<String>>(5)?,
                "detail": row.get::<_, Option<String>>(6)?,
            }))
        })
        .map_err(internal)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(internal)?;
    debug!(items = items.len(), "audit log");
    Ok(Json(json!({ "items": items })))
}

fn internal(err: rusqlite::Error) -> ApiError {
    error!(error = %err, "database error");
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "db_error",
        format!("database error: {err}"),
    )
}

fn row_to_admin_skill(row: &rusqlite::Row<'_>, public_url: &str) -> rusqlite::Result<SkillRow> {
    let slug: String = row.get("slug")?;
    let latest: Option<String> = row.get("latest_version")?;
    Ok(SkillRow {
        install_url: latest.map(|version| format!("{public_url}/skills/{slug}/{version}")),
        slug,
        name: row.get("name")?,
        owner_tenant: row.get("owner_tenant")?,
        owner_name: row.get("owner_name")?,
        description: crate::api::row_description(row)?,
        visibility: row.get("visibility")?,
        latest_version: row.get("latest_version")?,
        created_at: row.get("created_at")?,
        withdrawn_at: row.get("withdrawn_at")?,
        takedown_at: row.get("takedown_at")?,
        takedown_reason: row.get("takedown_reason")?,
        versions: row.get("versions")?,
        size_bytes: row.get("size_bytes")?,
    })
}

#[allow(dead_code)]
fn unused_salt() -> SaltString {
    SaltString::generate(&mut OsRng)
}
