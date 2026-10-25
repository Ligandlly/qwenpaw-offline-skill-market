//! Tenant-facing API. Every route here is called by the QwenPaw plugin
//! (browser → plugin route → backend), so `X-Tenant-Id` is always present.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tracing::{debug, error, info, warn};

use crate::auth::TenantId;
use crate::db::{audit, now};
use crate::limits::{self, Reject, MANIFEST};
use crate::state::Shared;

// ---------------------------------------------------------------- errors ---

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", what)
    }

    fn db(err: rusqlite::Error) -> Self {
        error!(error = %err, "database error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("database error: {err}"),
        )
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
}

impl From<Reject> for ApiError {
    fn from(reject: Reject) -> Self {
        Self::new(StatusCode::BAD_REQUEST, reject.code, reject.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ----------------------------------------------------------------- models ---

#[derive(Debug, Clone, Serialize)]
pub struct SkillRow {
    pub slug: String,
    pub name: String,
    pub owner_tenant: String,
    pub visibility: String,
    pub latest_version: Option<String>,
    pub created_at: i64,
    pub withdrawn_at: Option<i64>,
    pub takedown_at: Option<i64>,
    pub takedown_reason: Option<String>,
    pub versions: i64,
    pub size_bytes: i64,
    pub install_url: Option<String>,
}

fn row_to_skill(row: &Row<'_>, public_url: &str) -> rusqlite::Result<SkillRow> {
    let slug: String = row.get("slug")?;
    let latest: Option<String> = row.get("latest_version")?;
    let install_url = latest
        .as_ref()
        .map(|version| format!("{public_url}/skills/{slug}/{version}"));
    Ok(SkillRow {
        slug,
        name: row.get("name")?,
        owner_tenant: row.get("owner_tenant")?,
        visibility: row.get("visibility")?,
        latest_version: latest,
        created_at: row.get("created_at")?,
        withdrawn_at: row.get("withdrawn_at")?,
        takedown_at: row.get("takedown_at")?,
        takedown_reason: row.get("takedown_reason")?,
        versions: row.get("versions")?,
        size_bytes: row.get("size_bytes")?,
        install_url,
    })
}

const SKILL_SELECT: &str = r#"
SELECT s.slug, s.name, s.owner_tenant, s.visibility, s.latest_version,
       s.created_at, s.withdrawn_at, s.takedown_at, s.takedown_reason,
       COALESCE((SELECT COUNT(*) FROM skill_versions v WHERE v.slug = s.slug), 0) AS versions,
       COALESCE((SELECT SUM(size_bytes) FROM skill_versions v WHERE v.slug = s.slug), 0) AS size_bytes
FROM skills s
"#;

// ---------------------------------------------------------------- handlers ---

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "one")]
    pub page: i64,
    #[serde(default = "twenty")]
    pub limit: i64,
}

fn one() -> i64 {
    1
}
fn twenty() -> i64 {
    20
}

/// GET /api/v1/limits — the plugin mirrors these instead of hard-coding them.
pub async fn get_limits(State(app): State<Shared>) -> Json<Value> {
    let l = &app.cfg.limits;
    Json(json!({
        "max_bundle_bytes": l.max_bundle_bytes,
        "max_files": l.max_files,
        "max_file_bytes": l.max_file_bytes,
        "manifest": MANIFEST,
    }))
}

/// GET /api/v1/skills — market listing: public + own private, active only.
pub async fn list_skills(
    State(app): State<Shared>,
    tenant: TenantId,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.clamp(1, 50);
    let offset = (q.page.max(1) - 1) * limit;
    let pattern = format!("%{}%", q.q.trim());

    let db = app.db();
    db.execute(
        "INSERT INTO tenants (id, first_seen, last_seen) VALUES (?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen",
        params![tenant.0, now()],
    )
    .map_err(ApiError::db)?;

    let sql = format!(
        "{SKILL_SELECT}
         WHERE s.takedown_at IS NULL AND s.withdrawn_at IS NULL
           AND (s.visibility = 'public' OR s.owner_tenant = ?1)
           AND (s.slug LIKE ?2 OR s.name LIKE ?2)
         ORDER BY s.created_at DESC
         LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let rows = stmt
        .query_map(params![tenant.0, pattern, limit, offset], |row| {
            row_to_skill(row, &app.cfg.public_url)
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;

    debug!(
        tenant = %tenant.0,
        q = %q.q,
        page = q.page,
        limit,
        items = rows.len(),
        "market listing"
    );
    Ok(Json(
        json!({ "page": q.page.max(1), "limit": limit, "items": rows }),
    ))
}

/// GET /api/v1/skills/mine — everything the tenant owns, including withdrawn.
pub async fn list_mine(State(app): State<Shared>, tenant: TenantId) -> ApiResult<Json<Value>> {
    let db = app.db();
    let sql = format!("{SKILL_SELECT} WHERE s.owner_tenant = ?1 ORDER BY s.created_at DESC");
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let rows = stmt
        .query_map(params![tenant.0], |row| {
            row_to_skill(row, &app.cfg.public_url)
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;
    debug!(tenant = %tenant.0, items = rows.len(), "own skills");
    Ok(Json(json!({ "items": rows })))
}

/// GET /api/v1/skills/{slug}/status — drives the "已下架" badge on installs.
pub async fn skill_status(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let db = app.db();
    let mut stmt = db
        .prepare(
            "SELECT slug, name, owner_tenant, visibility, latest_version, withdrawn_at,
                    takedown_at, takedown_reason
             FROM skills WHERE slug = ?1",
        )
        .map_err(ApiError::db)?;
    let mut rows = stmt.query(params![slug]).map_err(ApiError::db)?;
    let Some(row) = rows.next().map_err(ApiError::db)? else {
        debug!(tenant = %tenant.0, slug = %slug, "status: unknown skill");
        return Err(ApiError::not_found("unknown skill"));
    };
    let owner: String = row.get("owner_tenant").map_err(ApiError::db)?;
    let visibility: String = row.get("visibility").map_err(ApiError::db)?;
    if owner != tenant.0 && visibility != "public" {
        debug!(
            tenant = %tenant.0,
            slug = %slug,
            owner = %owner,
            visibility = %visibility,
            "status: not visible to tenant"
        );
        return Err(ApiError::not_found("unknown skill"));
    }
    let withdrawn_at: Option<i64> = row.get("withdrawn_at").map_err(ApiError::db)?;
    let takedown_at: Option<i64> = row.get("takedown_at").map_err(ApiError::db)?;
    let state = if takedown_at.is_some() {
        "taken_down"
    } else if withdrawn_at.is_some() {
        "withdrawn"
    } else {
        "active"
    };
    debug!(tenant = %tenant.0, slug = %slug, state = %state, "status");
    Ok(Json(json!({
        "slug": row.get::<_, String>("slug").map_err(ApiError::db)?,
        "name": row.get::<_, String>("name").map_err(ApiError::db)?,
        "latest_version": row.get::<_, Option<String>>("latest_version").map_err(ApiError::db)?,
        "state": state,
        "takedown_reason": row.get::<_, Option<String>>("takedown_reason").map_err(ApiError::db)?,
    })))
}

#[derive(Debug, Deserialize)]
pub struct PublishRequest {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub version: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    pub files: BTreeMap<String, String>,
}

fn default_visibility() -> String {
    "public".to_string()
}

#[derive(Debug, Deserialize)]
pub struct LookupQuery {
    /// Comma-separated display names, as the QwenPaw host reports them.
    #[serde(default)]
    pub names: String,
}

/// GET /api/v1/skills/lookup?names=a,b — display name -> market slug + state.
///
/// The host stores hub installs under the *display* name and its skill list
/// does not expose where a skill came from, so the plugin needs this to map an
/// installed skill back to our slug. Unlike the market listing this includes
/// withdrawn and taken-down skills: that is the whole point.
pub async fn lookup_skills(
    State(app): State<Shared>,
    tenant: TenantId,
    Query(query): Query<LookupQuery>,
) -> ApiResult<Json<Value>> {
    let wanted: Vec<&str> = query
        .names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .take(100)
        .collect();
    if wanted.is_empty() {
        return Ok(Json(json!({ "items": [] })));
    }

    let placeholders = (0..wanted.len())
        .map(|index| format!("?{}", index + 2))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT slug, name, owner_tenant, visibility, latest_version,
                withdrawn_at, takedown_at, takedown_reason
         FROM skills
         WHERE name IN ({placeholders}) AND (visibility = 'public' OR owner_tenant = ?1)"
    );

    let db = app.db();
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(tenant.0.clone())];
    for name in &wanted {
        params.push(Box::new((*name).to_string()));
    }
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let items = stmt
        .query_map(refs.as_slice(), |row| {
            let takedown_at: Option<i64> = row.get("takedown_at")?;
            let withdrawn_at: Option<i64> = row.get("withdrawn_at")?;
            let state = if takedown_at.is_some() {
                "taken_down"
            } else if withdrawn_at.is_some() {
                "withdrawn"
            } else {
                "active"
            };
            Ok(json!({
                "slug": row.get::<_, String>("slug")?,
                "name": row.get::<_, String>("name")?,
                "owner_tenant": row.get::<_, String>("owner_tenant")?,
                "visibility": row.get::<_, String>("visibility")?,
                "latest_version": row.get::<_, Option<String>>("latest_version")?,
                "state": state,
                "takedown_reason": row.get::<_, Option<String>>("takedown_reason")?,
                "install_url": match row.get::<_, Option<String>>("latest_version")? {
                    Some(version) => format!("{}/skills/{}/{}", app.cfg.public_url, row.get::<_, String>("slug")?, version),
                    None => String::new(),
                },
            }))
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;

    debug!(
        tenant = %tenant.0,
        wanted = wanted.len(),
        found = items.len(),
        "lookup"
    );
    Ok(Json(json!({ "items": items })))
}

/// POST /api/v1/skills — publish an immutable version.
pub async fn publish(
    State(app): State<Shared>,
    tenant: TenantId,
    Json(body): Json<PublishRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if !matches!(body.visibility.as_str(), "public" | "tenant") {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_visibility",
            "visibility must be 'public' or 'tenant'",
        ));
    }
    if !limits::valid_version(&body.version) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_version",
            "version must be 1-32 chars of [0-9A-Za-z.+-] and start alphanumeric",
        ));
    }
    let slug = match body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(slug) => slug.to_string(),
        None => derive_slug(&body.name).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_slug",
                "cannot derive a slug from the name; pass an explicit lowercase ascii slug",
            )
        })?,
    };
    if !limits::valid_slug(&slug) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_slug",
            "slug must be 1-64 chars of [a-z0-9-] and start alphanumeric",
        ));
    }

    let checked = match limits::check_bundle(&body.name, &body.files, &app.cfg.limits) {
        Ok(checked) => checked,
        Err(reject) => {
            warn!(
                tenant = %tenant.0,
                slug = %slug,
                version = %body.version,
                code = reject.code,
                reason = %reject.message,
                "publish rejected by gate"
            );
            return Err(reject.into());
        }
    };

    let db = app.db();
    let actor = tenant.actor();
    db.execute(
        "INSERT INTO tenants (id, first_seen, last_seen) VALUES (?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen",
        params![tenant.0, now()],
    )
    .map_err(ApiError::db)?;

    let existing: Option<(String, Option<i64>)> = db
        .query_row(
            "SELECT owner_tenant, takedown_at FROM skills WHERE slug = ?1",
            params![slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    if let Some((owner, takedown_at)) = &existing {
        if owner != &tenant.0 {
            warn!(
                tenant = %tenant.0,
                slug = %slug,
                owner = %owner,
                "publish rejected: slug belongs to another tenant"
            );
            return Err(ApiError::forbidden(format!(
                "slug '{slug}' already belongs to another tenant"
            )));
        }
        if takedown_at.is_some() {
            warn!(
                tenant = %tenant.0,
                slug = %slug,
                "publish rejected: skill is taken down"
            );
            return Err(ApiError::conflict(
                "taken_down",
                "this skill was taken down by an administrator; publishing is blocked",
            ));
        }
    }

    let tx = db.unchecked_transaction().map_err(ApiError::db)?;
    tx.execute(
        "INSERT INTO skills (slug, name, owner_tenant, visibility, latest_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(slug) DO UPDATE SET name = excluded.name, visibility = excluded.visibility",
        params![
            slug,
            checked.name,
            tenant.0,
            body.visibility,
            body.version,
            now()
        ],
    )
    .map_err(ApiError::db)?;

    let inserted = tx.execute(
        "INSERT OR IGNORE INTO skill_versions
           (slug, version, sha256, size_bytes, file_count, bundle_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            slug,
            body.version,
            checked.sha256,
            checked.size_bytes as i64,
            checked.files.len() as i64,
            serde_json::to_string(&checked.files).unwrap_or_default(),
            now()
        ],
    );
    let inserted = match inserted {
        Ok(n) => n,
        Err(err) => return Err(ApiError::db(err)),
    };
    if inserted == 0 {
        warn!(
            tenant = %tenant.0,
            slug = %slug,
            version = %body.version,
            "publish rejected: version already exists"
        );
        return Err(ApiError::conflict(
            "version_exists",
            format!("version {} already exists and is immutable", body.version),
        ));
    }

    // latest pointer: only move forward (a late publish of an old version must
    // not roll the market back), and clear an owner-initiated withdrawal.
    let current: Option<String> = tx
        .query_row(
            "SELECT latest_version FROM skills WHERE slug = ?1",
            params![slug],
            |row| row.get(0),
        )
        .map_err(ApiError::db)?;
    let advance = current
        .as_deref()
        .map(|cur| compare_versions(&body.version, cur).is_gt())
        .unwrap_or(true);
    if advance {
        tx.execute(
            "UPDATE skills SET latest_version = ?2 WHERE slug = ?1",
            params![slug, body.version],
        )
        .map_err(ApiError::db)?;
    }
    tx.execute(
        "UPDATE skills SET withdrawn_at = NULL, withdrawn_by = NULL
         WHERE slug = ?1 AND withdrawn_at IS NOT NULL",
        params![slug],
    )
    .map_err(ApiError::db)?;

    audit(
        &tx,
        &actor,
        "publish",
        Some(&slug),
        Some(&body.version),
        Some(&format!(
            "{} files, {} bytes, visibility={}",
            checked.files.len(),
            checked.size_bytes,
            body.visibility
        )),
    )
    .map_err(ApiError::db)?;
    tx.commit().map_err(ApiError::db)?;

    info!(
        tenant = %tenant.0,
        slug = %slug,
        version = %body.version,
        visibility = %body.visibility,
        files = checked.files.len(),
        bytes = checked.size_bytes,
        sha256 = %checked.sha256,
        latest = advance,
        "published"
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "slug": slug,
            "version": body.version,
            "sha256": checked.sha256,
            "size_bytes": checked.size_bytes,
            "file_count": checked.files.len(),
            "latest_version": if advance { json!(body.version) } else { json!(current) },
            "install_url": format!("{}/skills/{}/{}", app.cfg.public_url, slug, body.version),
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct WithdrawRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// POST /api/v1/skills/{slug}/withdraw — owner may pull its own skill.
pub async fn withdraw(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
    body: Option<Json<WithdrawRequest>>,
) -> ApiResult<Json<Value>> {
    let reason = body.and_then(|Json(b)| b.reason);
    let db = app.db();
    let (owner, takedown_at): (String, Option<i64>) = db
        .query_row(
            "SELECT owner_tenant, takedown_at FROM skills WHERE slug = ?1",
            params![slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| ApiError::not_found("unknown skill"))?;
    if owner != tenant.0 {
        warn!(tenant = %tenant.0, slug = %slug, "withdraw rejected: not the owner");
        return Err(ApiError::forbidden("only the owning tenant may withdraw"));
    }
    if takedown_at.is_some() {
        warn!(
            tenant = %tenant.0,
            slug = %slug,
            "withdraw rejected: skill is taken down"
        );
        return Err(ApiError::conflict(
            "taken_down",
            "already taken down by an administrator",
        ));
    }
    db.execute(
        "UPDATE skills SET withdrawn_at = ?2, withdrawn_by = ?3 WHERE slug = ?1",
        params![slug, now(), tenant.0],
    )
    .map_err(ApiError::db)?;
    info!(
        tenant = %tenant.0,
        slug = %slug,
        reason = reason.as_deref().unwrap_or("-"),
        "withdrawn"
    );
    audit(
        db_ref(&db),
        &tenant.actor(),
        "withdraw",
        Some(&slug),
        None,
        reason.as_deref(),
    )
    .map_err(ApiError::db)?;
    Ok(Json(json!({ "slug": slug, "state": "withdrawn" })))
}

/// POST /api/v1/skills/{slug}/restore — reversible unless an admin took it down.
pub async fn restore(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let db = app.db();
    let (owner, takedown_at): (String, Option<i64>) = db
        .query_row(
            "SELECT owner_tenant, takedown_at FROM skills WHERE slug = ?1",
            params![slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| ApiError::not_found("unknown skill"))?;
    if owner != tenant.0 {
        warn!(
            tenant = %tenant.0,
            slug = %slug,
            "restore rejected: not the owner"
        );
        return Err(ApiError::forbidden("only the owning tenant may restore"));
    }
    if takedown_at.is_some() {
        warn!(
            tenant = %tenant.0,
            slug = %slug,
            "restore rejected: skill is taken down"
        );
        return Err(ApiError::conflict(
            "taken_down",
            "an administrator took this skill down; it cannot be self-restored",
        ));
    }
    db.execute(
        "UPDATE skills SET withdrawn_at = NULL, withdrawn_by = NULL WHERE slug = ?1",
        params![slug],
    )
    .map_err(ApiError::db)?;
    info!(tenant = %tenant.0, slug = %slug, "restored");
    audit(
        db_ref(&db),
        &tenant.actor(),
        "restore",
        Some(&slug),
        None,
        None,
    )
    .map_err(ApiError::db)?;
    Ok(Json(json!({ "slug": slug, "state": "active" })))
}

/// GET /skills/{slug}/{version} (alias of the bundle route) — this is the URL
/// the QwenPaw `MarketSpec.fetch_bundle` is handed as `bundle_url`.
pub async fn fetch_bundle(
    State(app): State<Shared>,
    tenant: TenantId,
    Path((slug, version)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let db = app.db();
    let (owner, visibility, latest, takedown_at, withdrawn_at): (
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
    ) = db
        .query_row(
            "SELECT owner_tenant, visibility, latest_version, takedown_at, withdrawn_at
             FROM skills WHERE slug = ?1",
            params![slug],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|_| ApiError::new(StatusCode::GONE, "gone", "unknown skill"))?;

    if takedown_at.is_some() {
        debug!(
            tenant = %tenant.0,
            slug = %slug,
            "bundle fetch: taken down"
        );
        return Err(ApiError::new(
            StatusCode::GONE,
            "taken_down",
            "this skill was taken down by an administrator and can no longer be downloaded",
        ));
    }
    let is_owner = owner == tenant.0;
    if withdrawn_at.is_some() && !is_owner {
        debug!(
            tenant = %tenant.0,
            slug = %slug,
            "bundle fetch: withdrawn"
        );
        return Err(ApiError::new(
            StatusCode::GONE,
            "withdrawn",
            "the publisher withdrew this skill",
        ));
    }
    if visibility != "public" && !is_owner {
        debug!(
            tenant = %tenant.0,
            slug = %slug,
            visibility = %visibility,
            "bundle fetch: private skill"
        );
        return Err(ApiError::forbidden("this skill is private to its tenant"));
    }

    let resolved = if version == "latest" {
        latest.ok_or_else(|| ApiError::new(StatusCode::GONE, "gone", "no published version"))?
    } else {
        version
    };

    let (name, bundle_json): (String, String) = db
        .query_row(
            "SELECT s.name, v.bundle_json FROM skills s
             JOIN skill_versions v ON v.slug = s.slug
             WHERE s.slug = ?1 AND v.version = ?2",
            params![slug, resolved],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| ApiError::not_found("unknown version"))?;

    let files: BTreeMap<String, String> = serde_json::from_str(&bundle_json).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt_bundle",
            format!("stored bundle is unreadable: {err}"),
        )
    })?;

    debug!(
        tenant = %tenant.0,
        slug = %slug,
        version = %resolved,
        files = files.len(),
        bytes = files.values().map(|content| content.len()).sum::<usize>(),
        "bundle fetched"
    );
    Ok(Json(json!({
        "name": name,
        "version": resolved,
        "files": files,
        "source_url": format!("{}/skills/{}/{}", app.cfg.public_url, slug, resolved),
    })))
}

// ------------------------------------------------------------------ helpers ---

fn db_ref(db: &rusqlite::Connection) -> &rusqlite::Connection {
    db
}

fn derive_slug(name: &str) -> Option<String> {
    let slug: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() || !limits::valid_slug(&slug) {
        None
    } else {
        Some(slug)
    }
}

/// Naive compare: numeric segments compare numerically, everything else
/// lexicographically. Enough to keep `latest` from moving backwards.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |v: &str| -> Vec<String> { v.split(['.', '-', '+']).map(str::to_string).collect() };
    let (av, bv) = (split(a), split(b));
    for i in 0..av.len().max(bv.len()) {
        let x = av.get(i).cloned().unwrap_or_default();
        let y = bv.get(i).cloned().unwrap_or_default();
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(nx), Ok(ny)) => nx.cmp(&ny),
            _ => x.cmp(&y),
        };
        if ord.is_ne() {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_sanely() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("1.0.10", "1.0.9"), Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Equal);
        assert_eq!(compare_versions("2.0.0", "10.0.0"), Less);
        assert_eq!(compare_versions("1.0.1", "1.0"), Greater);
    }

    #[test]
    fn derives_slugs() {
        assert_eq!(derive_slug("My Skill!"), Some("my-skill".to_string()));
        assert_eq!(derive_slug("技能"), None);
    }
}
