//! Tenant-facing API. Every route here is called by the QwenPaw plugin
//! (browser → plugin route → backend), so `X-Tenant-Id` is always present.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use pinyin::ToPinyin;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tracing::{debug, error, info, warn};

use crate::auth::TenantId;
use crate::db::{audit, now};
use crate::limits::{self, FileData, Reject, MANIFEST};
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

    pub(crate) fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", what)
    }

    pub(crate) fn db(err: rusqlite::Error) -> Self {
        error!(error = %err, "database error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("数据库错误：{err}"),
        )
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub(crate) fn conflict(code: &'static str, message: impl Into<String>) -> Self {
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

pub(crate) type ApiResult<T> = Result<T, ApiError>;

// ----------------------------------------------------------------- models ---

#[derive(Debug, Clone, Serialize)]
pub struct SkillRow {
    pub slug: String,
    pub name: String,
    /// `description` from the latest version's `SKILL.md` frontmatter, so the
    /// 市场 listing can show the same 描述 the host shows for installs.
    pub description: Option<String>,
    pub owner_tenant: String,
    /// Publisher name the tenant reported, or `null` for skills published
    /// before anyone reported one. Display only; never used for access.
    pub owner_name: Option<String>,
    pub visibility: String,
    pub latest_version: Option<String>,
    pub created_at: i64,
    /// Newest version upload time, falling back to the entry's `created_at`.
    /// Drives the market's "最近更新" sort.
    pub updated_at: i64,
    pub download_count: i64,
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
        description: row_description(row)?,
        owner_tenant: row.get("owner_tenant")?,
        owner_name: row.get("owner_name")?,
        visibility: row.get("visibility")?,
        latest_version: latest,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        download_count: row.get("download_count")?,
        withdrawn_at: row.get("withdrawn_at")?,
        takedown_at: row.get("takedown_at")?,
        takedown_reason: row.get("takedown_reason")?,
        versions: row.get("versions")?,
        size_bytes: row.get("size_bytes")?,
        install_url,
    })
}

/// Description for a row produced by a select carrying the `manifest` column
/// (see [`SKILL_SELECT`]).
pub(crate) fn row_description(row: &Row<'_>) -> rusqlite::Result<Option<String>> {
    Ok(row
        .get::<_, Option<String>>("manifest")?
        .as_deref()
        .and_then(limits::manifest_description))
}

/// `description` is not a column on purpose: it lives in the latest version's
/// `SKILL.md`, which is the only source that cannot fall out of sync — and it
/// works for rows published before the column existed. The key needs quoting:
/// `$.SKILL.md` would be read as a nested path.
const SKILL_SELECT: &str = r#"
SELECT s.slug, s.name, s.owner_tenant, t.display_name AS owner_name, s.visibility,
       s.latest_version,
       s.created_at, s.withdrawn_at, s.takedown_at, s.takedown_reason,
       s.download_count,
       COALESCE((SELECT MAX(v.created_at) FROM skill_versions v WHERE v.slug = s.slug),
                s.created_at) AS updated_at,
       (SELECT json_extract(v.bundle_json, '$."SKILL.md"')
          FROM skill_versions v
         WHERE v.slug = s.slug AND v.version = s.latest_version) AS manifest,
       COALESCE((SELECT COUNT(*) FROM skill_versions v WHERE v.slug = s.slug), 0) AS versions,
       COALESCE((SELECT SUM(size_bytes) FROM skill_versions v WHERE v.slug = s.slug), 0) AS size_bytes
FROM skills s
LEFT JOIN tenants t ON t.id = s.owner_tenant
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
    /// `created` (default) | `downloads` | `updated`.
    #[serde(default)]
    pub sort: String,
}

/// Whitelisted sort keys; anything else degrades to the historical order.
pub(crate) fn normalize_sort(sort: &str) -> &'static str {
    match sort {
        "downloads" => "downloads",
        "updated" => "updated",
        _ => "created",
    }
}

/// Whitelisted ORDER BY for the market listing. Never interpolate user input
/// into SQL; unknown values fall back to the historical newest-first order.
pub(crate) fn skill_order(sort: &str) -> &'static str {
    match normalize_sort(sort) {
        "downloads" => "s.download_count DESC, s.created_at DESC",
        "updated" => "updated_at DESC, s.created_at DESC",
        _ => "s.created_at DESC",
    }
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
    let order = skill_order(&q.sort);

    let db = app.db();
    db.execute(
        "INSERT INTO tenants (id, first_seen, last_seen) VALUES (?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen",
        params![tenant.0, now()],
    )
    .map_err(ApiError::db)?;

    let total: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM skills s
             WHERE s.takedown_at IS NULL AND s.withdrawn_at IS NULL
               AND (s.visibility = 'public' OR s.owner_tenant = ?1)
               AND (s.slug LIKE ?2 OR s.name LIKE ?2)",
            params![tenant.0, pattern],
            |row| row.get(0),
        )
        .map_err(ApiError::db)?;

    let sql = format!(
        "{SKILL_SELECT}
         WHERE s.takedown_at IS NULL AND s.withdrawn_at IS NULL
           AND (s.visibility = 'public' OR s.owner_tenant = ?1)
           AND (s.slug LIKE ?2 OR s.name LIKE ?2)
         ORDER BY {order}
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
    Ok(Json(json!({
        "page": q.page.max(1),
        "limit": limit,
        "total": total,
        "sort": normalize_sort(&q.sort),
        "items": rows
    })))
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
        return Err(ApiError::not_found("技能不存在"));
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
        return Err(ApiError::not_found("技能不存在"));
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
    /// Optional publisher name, resolved by the plugin from the tenant's own
    /// console login. Cosmetic: it labels the listing, it grants nothing.
    #[serde(default)]
    pub publisher_name: Option<String>,
    pub files: BTreeMap<String, FileData>,
}

fn default_visibility() -> String {
    "public".to_string()
}

#[derive(Debug, Deserialize)]
pub struct SlugQuery {
    #[serde(default)]
    pub name: String,
}

/// GET /api/v1/slug?name=... — the identifier a publish would derive.
///
/// The publish page calls this to prefill 标识符 (Chinese names included), so the
/// derivation lives in exactly one place: here. `slug` is `null` when the name
/// yields nothing usable; the page then leaves the field empty.
pub async fn preview_slug(Query(query): Query<SlugQuery>) -> Json<Value> {
    Json(json!({ "name": query.name, "slug": derive_slug(&query.name) }))
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
    let publisher = clean_publisher_name(body.publisher_name.as_deref());
    if !matches!(body.visibility.as_str(), "public" | "tenant") {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_visibility",
            "可见范围只能是 public 或 tenant",
        ));
    }
    if !limits::valid_version(&body.version) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_version",
            "版本号需为 1-32 位，只能包含 0-9 A-Z a-z . + -，且以字母或数字开头",
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
                "无法从名称推导出标识符，请手动填写一个只含小写字母、数字和 - 的标识符",
            )
        })?,
    };
    if !limits::valid_slug(&slug) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_slug",
            "标识符需为 1-64 位，只能包含小写字母、数字和 -，且以字母或数字开头",
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
    // `publisher` is only written when the plugin actually sent one: a publish
    // without it must not wipe the name an earlier one recorded.
    db.execute(
        "INSERT INTO tenants (id, first_seen, last_seen, display_name) VALUES (?1, ?2, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen,
             display_name = COALESCE(excluded.display_name, tenants.display_name)",
        params![tenant.0, now(), publisher],
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
                "标识符 '{slug}' 已被其他团队占用"
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
                "该技能已被管理员下架，不能再发布",
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
            format!(
                "版本 {} 已存在且不可修改，请不要重复发布同一个版本号",
                body.version
            ),
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
        publisher = publisher.as_deref().unwrap_or("-"),
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

/// Bump the patch component of ``x.y.z`` for the "publish a new version" hint.
/// Anything unexpected (a tag like ``v1``, a date, four components) yields
/// ``None`` and the form keeps whatever the user typed.
fn suggest_next_version(latest: &str) -> Option<String> {
    let parts: Vec<&str> = latest.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut numbers = [0u64; 3];
    for (slot, part) in numbers.iter_mut().zip(parts.iter()) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(format!(
        "{}.{}.{}",
        numbers[0],
        numbers[1],
        numbers[2].checked_add(1)?
    ))
}

/// The slate a publish would land on, as the dry run reports it back.
struct ExistingSkill {
    name: String,
    owner_tenant: String,
    latest_version: Option<String>,
    takedown_at: Option<i64>,
    withdrawn_at: Option<i64>,
}

/// POST /api/v1/skills/validate — dry run of a publish.
///
/// The publish form calls this on the archive the user just picked, so every
/// problem (bundle gate, slug, version collision, ownership) shows up as a list
/// before they hit 发布 instead of as one 4xx after it. Read-only: no rows are
/// written, nothing is audited, downloads do not move.
pub async fn validate(
    State(app): State<Shared>,
    tenant: TenantId,
    Json(body): Json<PublishRequest>,
) -> ApiResult<Json<Value>> {
    let mut problems: Vec<Value> = Vec::new();
    let mut push = |code: &str, message: String| {
        problems.push(json!({ "code": code, "message": message }));
    };

    if !matches!(body.visibility.as_str(), "public" | "tenant") {
        push(
            "invalid_visibility",
            "可见范围只能是 public 或 tenant".into(),
        );
    }
    if !limits::valid_version(&body.version) {
        push(
            "invalid_version",
            "版本号需为 1-32 位，只能包含 0-9 A-Z a-z . + -，且以字母或数字开头".into(),
        );
    }

    let slug = match body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(slug) => Some(slug.to_string()),
        None => derive_slug(&body.name),
    };
    match &slug {
        None => push(
            "invalid_slug",
            "无法从名称推导出标识符，请手动填写一个只含小写字母、数字和 - 的标识符".into(),
        ),
        Some(slug) if !limits::valid_slug(slug) => push(
            "invalid_slug",
            "标识符需为 1-64 位，只能包含小写字母、数字和 -，且以字母或数字开头".into(),
        ),
        Some(_) => {}
    }

    let (_checked, rejects) = limits::review_bundle(&body.name, &body.files, &app.cfg.limits);
    for reject in rejects {
        push(reject.code, reject.message);
    }

    // Slate-level checks: only meaningful once the slug is a real string, and
    // the same rules the publish path applies.
    let mut existing: Option<Value> = None;
    let mut next_version: Option<String> = None;
    if let Some(slug) = slug.as_deref().filter(|s| limits::valid_slug(s)) {
        let db = app.db();
        let row: Option<ExistingSkill> = db
            .query_row(
                "SELECT name, owner_tenant, latest_version, takedown_at, withdrawn_at
                 FROM skills WHERE slug = ?1",
                params![slug],
                |row| {
                    Ok(ExistingSkill {
                        name: row.get(0)?,
                        owner_tenant: row.get(1)?,
                        latest_version: row.get(2)?,
                        takedown_at: row.get(3)?,
                        withdrawn_at: row.get(4)?,
                    })
                },
            )
            .ok();
        if let Some(ExistingSkill {
            name,
            owner_tenant: owner,
            latest_version: latest,
            takedown_at,
            withdrawn_at,
        }) = row
        {
            let mine = owner == tenant.0;
            if mine {
                next_version = latest.as_deref().and_then(suggest_next_version);
                if takedown_at.is_some() {
                    push("taken_down", "该技能已被管理员下架，不能再发布".into());
                }
            } else {
                push("slug_taken", format!("标识符 '{slug}' 已被其他团队占用"));
            }
            existing = Some(json!({
                "name": name,
                "owner_tenant": owner,
                "latest_version": latest,
                "state": if takedown_at.is_some() {
                    "taken_down"
                } else if withdrawn_at.is_some() {
                    "withdrawn"
                } else {
                    "active"
                },
                "mine": mine,
            }));
        }
        let taken: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM skill_versions WHERE slug = ?1 AND version = ?2",
                params![slug, body.version],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if taken > 0 {
            push(
                "version_exists",
                format!(
                    "版本 {} 已存在且不可修改，请不要重复发布同一个版本号",
                    body.version
                ),
            );
        }
    }

    // A manifest that parses is worth showing: the page previews the name and
    // description the host will display after install.
    let manifest = body
        .files
        .get(MANIFEST)
        .and_then(|content| content.as_text())
        .and_then(|text| limits::manifest_meta(text).ok())
        .map(|(name, description)| json!({ "name": name, "description": description }));

    let file_count = body.files.len();
    let size_bytes: usize = body.files.values().map(|content| content.len()).sum();
    let files: Vec<Value> = body
        .files
        .iter()
        .map(|(path, content)| json!({ "path": path, "size_bytes": content.len() }))
        .collect();

    debug!(
        tenant = %tenant.0,
        slug = slug.as_deref().unwrap_or("-"),
        files = file_count,
        problems = problems.len(),
        "validate"
    );

    Ok(Json(json!({
        "ok": problems.is_empty(),
        "slug": slug,
        "name": body.name.trim(),
        "version": body.version,
        "visibility": body.visibility,
        "file_count": file_count,
        "size_bytes": size_bytes,
        "files": files,
        "manifest": manifest,
        "existing": existing,
        "next_version": next_version,
        "limits": {
            "max_files": app.cfg.limits.max_files,
            "max_file_bytes": app.cfg.limits.max_file_bytes,
            "max_bundle_bytes": app.cfg.limits.max_bundle_bytes,
        },
        "problems": problems,
    })))
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
        .map_err(|_| ApiError::not_found("技能不存在"))?;
    if owner != tenant.0 {
        warn!(tenant = %tenant.0, slug = %slug, "withdraw rejected: not the owner");
        return Err(ApiError::forbidden("只有发布者所在团队才能下架"));
    }
    if takedown_at.is_some() {
        warn!(
            tenant = %tenant.0,
            slug = %slug,
            "withdraw rejected: skill is taken down"
        );
        return Err(ApiError::conflict("taken_down", "该条目已被管理员下架"));
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
        .map_err(|_| ApiError::not_found("技能不存在"))?;
    if owner != tenant.0 {
        warn!(
            tenant = %tenant.0,
            slug = %slug,
            "restore rejected: not the owner"
        );
        return Err(ApiError::forbidden("只有发布者所在团队才能重新上架"));
    }
    if takedown_at.is_some() {
        warn!(
            tenant = %tenant.0,
            slug = %slug,
            "restore rejected: skill is taken down"
        );
        return Err(ApiError::conflict(
            "taken_down",
            "该技能由管理员下架，不能自行恢复上架",
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
        .map_err(|_| ApiError::new(StatusCode::GONE, "gone", "技能不存在"))?;

    if takedown_at.is_some() {
        debug!(
            tenant = %tenant.0,
            slug = %slug,
            "bundle fetch: taken down"
        );
        return Err(ApiError::new(
            StatusCode::GONE,
            "taken_down",
            "该技能已被管理员下架，无法再下载",
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
            "发布者已撤回该技能",
        ));
    }
    if visibility != "public" && !is_owner {
        debug!(
            tenant = %tenant.0,
            slug = %slug,
            visibility = %visibility,
            "bundle fetch: private skill"
        );
        return Err(ApiError::forbidden("该技能仅对所属团队可见"));
    }

    let resolved = if version == "latest" {
        latest.ok_or_else(|| ApiError::new(StatusCode::GONE, "gone", "还没有已发布的版本"))?
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
        .map_err(|_| ApiError::not_found("版本不存在"))?;

    let files: BTreeMap<String, FileData> = serde_json::from_str(&bundle_json).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt_bundle",
            format!("存储的包无法读取：{err}"),
        )
    })?;

    if let Err(err) = db.execute(
        "UPDATE skills SET download_count = download_count + 1 WHERE slug = ?1",
        params![slug],
    ) {
        // A counter must never fail an install that already succeeded.
        warn!(slug = %slug, error = %err, "download counter update failed");
    }

    debug!(
        tenant = %tenant.0,
        slug = %slug,
        version = %resolved,
        files = files.len(),
        bytes = files.values().map(FileData::len).sum::<usize>(),
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

/// Publisher names are labels, not identities: anything unusable is dropped
/// instead of failing the publish (ownership is the tenant header, always).
pub(crate) fn clean_publisher_name(raw: Option<&str>) -> Option<String> {
    let cleaned: String = raw?.chars().filter(|c| !c.is_control()).take(64).collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn db_ref(db: &rusqlite::Connection) -> &rusqlite::Connection {
    db
}

/// Identifier a publish derives from a display name: `My Skill!` -> `my-skill`,
/// `周报助手` -> `zhoubaozhushou`.
///
/// Han characters become plain (tone-less) pinyin, syllables joined, so a
/// Chinese name still gets a stable lowercase-ASCII slug instead of forcing the
/// page to invent one. Everything else that is not `[a-z0-9]` separates words,
/// repeated separators collapse, and the result is capped at `MAX_SLUG_LEN`
/// (truncated on a separator) so long names do not fail the publish.
pub(crate) fn derive_slug(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(name.len());
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if let Some(pinyin) = c.to_pinyin() {
            slug.push_str(pinyin.plain());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.len() > limits::MAX_SLUG_LEN {
        slug[..limits::MAX_SLUG_LEN].trim_end_matches('-')
    } else {
        slug
    };
    if slug.is_empty() || !limits::valid_slug(slug) {
        None
    } else {
        Some(slug.to_string())
    }
}

/// Naive compare: numeric segments compare numerically, everything else
/// lexicographically. Enough to keep `latest` from moving backwards.
pub(crate) fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
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
    fn suggests_the_next_patch_version() {
        assert_eq!(suggest_next_version("1.0.0").as_deref(), Some("1.0.1"));
        assert_eq!(suggest_next_version("2.2.2").as_deref(), Some("2.2.3"));
        assert_eq!(suggest_next_version("1.0.9").as_deref(), Some("1.0.10"));
        // Anything non-semver leaves the field alone.
        for odd in ["v1", "1.0", "1.0.0.post1", "latest", ""] {
            assert_eq!(suggest_next_version(odd), None, "{odd}");
        }
    }

    #[test]
    fn derives_slugs() {
        assert_eq!(derive_slug("My Skill!"), Some("my-skill".to_string()));
        assert_eq!(derive_slug("  My   Skill  "), Some("my-skill".into()));
        assert_eq!(derive_slug("v1.2 tool"), Some("v1-2-tool".into()));
        // Chinese names transliterate to plain pinyin instead of failing.
        assert_eq!(derive_slug("技能"), Some("jineng".into()));
        assert_eq!(derive_slug("周报助手"), Some("zhoubaozhushou".into()));
        assert_eq!(
            derive_slug("周报助手 Pro"),
            Some("zhoubaozhushou-pro".into())
        );
        assert_eq!(derive_slug("技能（测试）"), Some("jineng-ceshi".into()));
        // Nothing usable -> None (publish then demands an explicit slug).
        assert_eq!(derive_slug(""), None);
        assert_eq!(derive_slug("  "), None);
        assert_eq!(derive_slug("!!!"), None);
        // Long names are truncated on a separator, not rejected.
        let long = derive_slug(&"技能".repeat(40)).unwrap();
        assert!(limits::valid_slug(&long), "{long}");
        assert_eq!(long.len(), 64);
    }

    #[test]
    fn cleans_publisher_names_but_never_invents_one() {
        assert_eq!(clean_publisher_name(Some(" admin ")), Some("admin".into()));
        assert_eq!(clean_publisher_name(Some("张三")), Some("张三".into()));
        assert_eq!(
            clean_publisher_name(Some("\u{0}admin\n")),
            Some("admin".into())
        );
        assert_eq!(clean_publisher_name(Some("   ")), None);
        assert_eq!(clean_publisher_name(None), None);
        assert_eq!(
            clean_publisher_name(Some(&"x".repeat(200))).unwrap().len(),
            64
        );
    }
}
