//! Agent market: the same publish/list/withdraw/takedown shape as skills, but
//! for agent bundles (`manifest.json` + persona markdown + bundled skills).
//!
//! Security hardening and per-bundle quotas are deliberately deferred for the
//! MVP (see docs/AGENT_MARKET.md §9). What remains here is the structural
//! validation needed for a bundle to be installable at all: manifest shape,
//! path rules, declared-vs-present files, and each skill's `SKILL.md`
//! frontmatter (the host refuses installs without it).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use rusqlite::{params, Row};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use tracing::{debug, info, warn};

use crate::api::{clean_publisher_name, compare_versions, derive_slug, ApiError, ApiResult};
use crate::auth::{Admin, TenantId};
use crate::db::{audit_kind, now};
use crate::limits::{self, FileData, Reject};
use crate::state::Shared;
use sha2::{Digest, Sha256};

pub const AGENT_MANIFEST: &str = "manifest.json";

/// Root markdown files an agent bundle may carry. Everything else must live
/// under `skills/<name>/`.
pub const AGENT_MD_FILES: [&str; 5] = [
    "AGENTS.md",
    "SOUL.md",
    "PROFILE.md",
    "HEARTBEAT.md",
    "BOOTSTRAP.md",
];

const THINKING_LEVELS: [&str; 9] = [
    "inherit", "off", "minimal", "low", "medium", "high", "xhigh", "max", "budget",
];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub thinking_budget: Option<u64>,
    #[serde(default)]
    pub plan_enabled: bool,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentManifest {
    pub schema: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub profile: AgentProfile,
    #[serde(default)]
    pub md_files: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
}

fn default_language() -> String {
    "zh".to_string()
}

/// Normalised agent package that passed structural validation.
#[derive(Debug, Clone)]
pub struct CheckedAgentBundle {
    pub manifest: AgentManifest,
    pub files: BTreeMap<String, FileData>,
    pub sha256: String,
    pub size_bytes: usize,
    pub file_count: usize,
    pub skill_count: usize,
}

/// Structural gate. No size/file-count limits yet: the MVP only rejects
/// bundles that cannot be installed or could write outside a skill directory.
pub fn check_agent_bundle(
    files: &BTreeMap<String, FileData>,
) -> Result<CheckedAgentBundle, Reject> {
    if files.is_empty() {
        return Err(Reject::new("empty_bundle", "bundle contains no files"));
    }
    let manifest_file = files.get(AGENT_MANIFEST).ok_or_else(|| {
        Reject::new(
            "missing_manifest",
            format!("bundle must contain {AGENT_MANIFEST} at its root"),
        )
    })?;
    let manifest_text = manifest_file.as_text().ok_or_else(|| {
        Reject::new(
            "invalid_manifest",
            format!("{AGENT_MANIFEST} must be UTF-8 text"),
        )
    })?;
    let manifest: AgentManifest = serde_json::from_str(manifest_text).map_err(|err| {
        Reject::new(
            "invalid_manifest",
            format!("{AGENT_MANIFEST} is not valid JSON: {err}"),
        )
    })?;

    if manifest.schema != 1 {
        return Err(Reject::new(
            "unsupported_schema",
            format!(
                "unsupported manifest schema {}; expected 1",
                manifest.schema
            ),
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err(Reject::new("invalid_name", "agent name must not be empty"));
    }
    if manifest.language.trim().is_empty() {
        return Err(Reject::new(
            "invalid_language",
            "agent language must not be empty",
        ));
    }
    validate_profile(&manifest.profile)?;

    if manifest.md_files.is_empty() && manifest.skills.is_empty() {
        return Err(Reject::new(
            "empty_agent",
            "agent bundle needs at least one markdown file or bundled skill",
        ));
    }

    let mut md_set: BTreeSet<&str> = BTreeSet::new();
    for md in &manifest.md_files {
        if !AGENT_MD_FILES.contains(&md.as_str()) {
            return Err(Reject::new(
                "invalid_md_file",
                format!(
                    "{md}: only {} are allowed at the bundle root",
                    AGENT_MD_FILES.join(", ")
                ),
            ));
        }
        if !md_set.insert(md.as_str()) {
            return Err(Reject::new(
                "duplicate_md_file",
                format!("{md}: listed more than once"),
            ));
        }
        if !files.contains_key(md) {
            return Err(Reject::new(
                "missing_md_file",
                format!("{md}: listed in md_files but missing from the bundle"),
            ));
        }
    }

    let mut skill_set: BTreeSet<&str> = BTreeSet::new();
    for name in &manifest.skills {
        if !valid_skill_name(name) {
            return Err(Reject::new(
                "invalid_skill_name",
                format!(
                    "{name}: skill names must be non-empty, at most 128 bytes, and free of path separators or control characters"
                ),
            ));
        }
        if !skill_set.insert(name.as_str()) {
            return Err(Reject::new(
                "duplicate_skill",
                format!("{name}: listed more than once"),
            ));
        }
        let skill_manifest = format!("skills/{name}/SKILL.md");
        let content = files
            .get(&skill_manifest)
            .ok_or_else(|| {
                Reject::new(
                    "missing_skill_manifest",
                    format!("{skill_manifest}: declared skill has no SKILL.md"),
                )
            })?
            .as_text()
            .ok_or_else(|| {
                Reject::new(
                    "invalid_skill_manifest",
                    format!("{skill_manifest}: SKILL.md must be UTF-8 text"),
                )
            })?;
        limits::manifest_meta(content)?;
    }

    let mut size_bytes = 0usize;
    for path in files.keys() {
        limits::check_path(path)?;
        if path == AGENT_MANIFEST {
            continue;
        }
        if !path.contains('/') {
            if !md_set.contains(path.as_str()) {
                return Err(Reject::new(
                    "unexpected_root_file",
                    format!(
                        "{path}: only {AGENT_MANIFEST} and declared md_files may live at the root"
                    ),
                ));
            }
            continue;
        }
        let mut parts = path.split('/');
        let root = parts.next().unwrap_or_default();
        let skill = parts.next().unwrap_or_default();
        if root != "skills" {
            return Err(Reject::new(
                "unexpected_path",
                format!("{path}: non-markdown files must live under skills/<name>/"),
            ));
        }
        if !skill_set.contains(skill) {
            return Err(Reject::new(
                "undeclared_skill",
                format!("{path}: skill '{skill}' is not declared in manifest.skills"),
            ));
        }
        if parts.next().is_none() {
            return Err(Reject::new(
                "invalid_path",
                format!("{path}: a skill entry must be a file inside the skill directory"),
            ));
        }
    }
    size_bytes += files.values().map(FileData::len).sum::<usize>();

    Ok(CheckedAgentBundle {
        sha256: limits::digest(files),
        size_bytes,
        file_count: files.len(),
        skill_count: manifest.skills.len(),
        manifest,
        files: files.clone(),
    })
}

fn validate_profile(profile: &AgentProfile) -> Result<(), Reject> {
    if let Some(level) = profile.thinking_level.as_deref() {
        if !THINKING_LEVELS.contains(&level) {
            return Err(Reject::new(
                "invalid_profile",
                format!("unknown thinking_level '{level}'"),
            ));
        }
    }
    let budget_ok = match (profile.thinking_level.as_deref(), profile.thinking_budget) {
        (Some("budget"), Some(budget)) => budget > 0,
        (Some("budget"), None) => false,
        (Some(_), None) => true,
        (None, None) => true,
        _ => false,
    };
    if !budget_ok {
        return Err(Reject::new(
            "invalid_profile",
            "thinking_budget must be a positive integer and only with thinking_level=budget",
        ));
    }
    for tool in &profile.disabled_tools {
        let valid = !tool.is_empty()
            && tool.len() <= 64
            && tool
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            return Err(Reject::new(
                "invalid_profile",
                format!("invalid tool name '{tool}' in disabled_tools"),
            ));
        }
    }
    Ok(())
}

/// Mirror of the host's `normalize_skill_dir_name`: any non-empty directory
/// name without path separators or control characters is installable. Do not
/// impose a lowercase-ASCII style here — existing agents legitimately carry
/// names such as `QA_source_index`, and the host preserves them.
fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name != "."
        && name != ".."
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(|c| c.is_control())
}

/// Slug for an agent whose display name has no ASCII slug form (e.g. Chinese).
/// Stable across versions so republishing reuses the same market entry.
fn agent_slug(name: &str) -> String {
    if let Some(slug) = derive_slug(name) {
        return slug;
    }
    let digest = Sha256::digest(name.trim().as_bytes());
    let suffix: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("agent-{suffix}")
}

// ---------------------------------------------------------------- listings ---

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentRow {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub owner_tenant: String,
    pub owner_name: Option<String>,
    pub visibility: String,
    pub latest_version: Option<String>,
    pub created_at: i64,
    pub withdrawn_at: Option<i64>,
    pub takedown_at: Option<i64>,
    pub takedown_reason: Option<String>,
    pub versions: i64,
    pub size_bytes: i64,
    pub skill_count: i64,
    pub bundle_url: Option<String>,
}

fn row_to_agent(row: &Row<'_>, public_url: &str) -> rusqlite::Result<AgentRow> {
    let slug: String = row.get("slug")?;
    let latest: Option<String> = row.get("latest_version")?;
    let bundle_url = latest
        .as_ref()
        .map(|version| format!("{public_url}/api/v1/agents/{slug}/{version}/bundle"));
    Ok(AgentRow {
        slug,
        name: row.get("name")?,
        description: row.get("description")?,
        owner_tenant: row.get("owner_tenant")?,
        owner_name: row.get("owner_name")?,
        visibility: row.get("visibility")?,
        latest_version: latest,
        created_at: row.get("created_at")?,
        withdrawn_at: row.get("withdrawn_at")?,
        takedown_at: row.get("takedown_at")?,
        takedown_reason: row.get("takedown_reason")?,
        versions: row.get("versions")?,
        size_bytes: row.get("size_bytes")?,
        skill_count: row.get("skill_count")?,
        bundle_url,
    })
}

const AGENT_SELECT: &str = r#"
SELECT a.slug, a.name, a.description, a.owner_tenant, t.display_name AS owner_name,
       a.visibility, a.latest_version, a.created_at, a.withdrawn_at,
       a.takedown_at, a.takedown_reason,
       COALESCE((SELECT COUNT(*) FROM agent_versions v WHERE v.slug = a.slug), 0) AS versions,
       COALESCE((SELECT SUM(size_bytes) FROM agent_versions v WHERE v.slug = a.slug), 0) AS size_bytes,
       COALESCE((SELECT skill_count FROM agent_versions v
                  WHERE v.slug = a.slug AND v.version = a.latest_version), 0) AS skill_count
FROM agents a
LEFT JOIN tenants t ON t.id = a.owner_tenant
"#;

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

/// GET /api/v1/agents
pub async fn list_agents(
    State(app): State<Shared>,
    tenant: TenantId,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = query.limit.clamp(1, 50);
    let offset = (query.page.max(1) - 1) * limit;
    let pattern = format!("%{}%", query.q.trim());

    let db = app.db();
    db.execute(
        "INSERT INTO tenants (id, first_seen, last_seen) VALUES (?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen",
        params![tenant.0, now()],
    )
    .map_err(ApiError::db)?;

    let sql = format!(
        "{AGENT_SELECT}
         WHERE a.takedown_at IS NULL AND a.withdrawn_at IS NULL
           AND (a.visibility = 'public' OR a.owner_tenant = ?1)
           AND (a.slug LIKE ?2 OR a.name LIKE ?2)
         ORDER BY a.created_at DESC
         LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let rows = stmt
        .query_map(params![tenant.0, pattern, limit, offset], |row| {
            row_to_agent(row, &app.cfg.public_url)
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;

    debug!(
        tenant = %tenant.0,
        q = %query.q,
        items = rows.len(),
        "agent listing"
    );
    Ok(Json(
        json!({ "page": query.page.max(1), "limit": limit, "items": rows }),
    ))
}

/// GET /api/v1/agents/mine
pub async fn list_mine(State(app): State<Shared>, tenant: TenantId) -> ApiResult<Json<Value>> {
    let db = app.db();
    let sql = format!("{AGENT_SELECT} WHERE a.owner_tenant = ?1 ORDER BY a.created_at DESC");
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let rows = stmt
        .query_map(params![tenant.0], |row| {
            row_to_agent(row, &app.cfg.public_url)
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;
    debug!(tenant = %tenant.0, items = rows.len(), "own agents");
    Ok(Json(json!({ "items": rows })))
}

/// GET /api/v1/agents/{slug}/status
pub async fn agent_status(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let db = app.db();
    let mut stmt = db
        .prepare(
            "SELECT slug, name, owner_tenant, visibility, latest_version,
                    withdrawn_at, takedown_at, takedown_reason
             FROM agents WHERE slug = ?1",
        )
        .map_err(ApiError::db)?;
    let mut rows = stmt.query(params![slug]).map_err(ApiError::db)?;
    let Some(row) = rows.next().map_err(ApiError::db)? else {
        return Err(ApiError::not_found("unknown agent"));
    };
    let owner: String = row.get("owner_tenant").map_err(ApiError::db)?;
    let visibility: String = row.get("visibility").map_err(ApiError::db)?;
    if owner != tenant.0 && visibility != "public" {
        return Err(ApiError::not_found("unknown agent"));
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
    Ok(Json(json!({
        "slug": row.get::<_, String>("slug").map_err(ApiError::db)?,
        "name": row.get::<_, String>("name").map_err(ApiError::db)?,
        "latest_version": row.get::<_, Option<String>>("latest_version").map_err(ApiError::db)?,
        "state": state,
        "takedown_reason": row.get::<_, Option<String>>("takedown_reason").map_err(ApiError::db)?,
    })))
}

// ---------------------------------------------------------------- publish ---

#[derive(Debug, Deserialize)]
pub struct PublishRequest {
    #[serde(default)]
    pub slug: Option<String>,
    pub version: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(default)]
    pub publisher_name: Option<String>,
    pub files: BTreeMap<String, FileData>,
}

fn default_visibility() -> String {
    "public".to_string()
}

/// POST /api/v1/agents — publish an immutable agent version.
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

    let checked = match check_agent_bundle(&body.files) {
        Ok(checked) => checked,
        Err(reject) => {
            warn!(
                tenant = %tenant.0,
                version = %body.version,
                code = reject.code,
                reason = %reject.message,
                "agent publish rejected by gate"
            );
            return Err(reject.into());
        }
    };

    let slug = match body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|slug| !slug.is_empty())
    {
        Some(slug) => slug.to_string(),
        None => agent_slug(&checked.manifest.name),
    };
    if !limits::valid_slug(&slug) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_slug",
            "slug must be 1-64 chars of [a-z0-9-] and start alphanumeric",
        ));
    }

    let publisher = clean_publisher_name(body.publisher_name.as_deref());
    let name = checked.manifest.name.trim().to_string();
    let description = checked.manifest.description.trim().to_string();

    let db = app.db();
    let actor = tenant.actor();
    db.execute(
        "INSERT INTO tenants (id, first_seen, last_seen, display_name) VALUES (?1, ?2, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen,
             display_name = COALESCE(excluded.display_name, tenants.display_name)",
        params![tenant.0, now(), publisher],
    )
    .map_err(ApiError::db)?;

    let existing: Option<(String, Option<i64>)> = db
        .query_row(
            "SELECT owner_tenant, takedown_at FROM agents WHERE slug = ?1",
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
                "agent publish rejected: slug belongs to another tenant"
            );
            return Err(ApiError::forbidden(format!(
                "slug '{slug}' already belongs to another tenant"
            )));
        }
        if takedown_at.is_some() {
            return Err(ApiError::conflict(
                "taken_down",
                "this agent was taken down by an administrator; publishing is blocked",
            ));
        }
    }

    let tx = db.unchecked_transaction().map_err(ApiError::db)?;
    tx.execute(
        "INSERT INTO agents (slug, name, description, owner_tenant, visibility, latest_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(slug) DO UPDATE SET name = excluded.name, visibility = excluded.visibility",
        params![
            slug,
            name,
            description,
            tenant.0,
            body.visibility,
            body.version,
            now()
        ],
    )
    .map_err(ApiError::db)?;

    let inserted = tx.execute(
        "INSERT OR IGNORE INTO agent_versions
           (slug, version, sha256, size_bytes, file_count, skill_count, bundle_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            slug,
            body.version,
            checked.sha256,
            checked.size_bytes as i64,
            checked.file_count as i64,
            checked.skill_count as i64,
            serde_json::to_string(&checked.files).unwrap_or_default(),
            now()
        ],
    );
    let inserted = match inserted {
        Ok(count) => count,
        Err(err) => return Err(ApiError::db(err)),
    };
    if inserted == 0 {
        warn!(
            tenant = %tenant.0,
            slug = %slug,
            version = %body.version,
            "agent publish rejected: version already exists"
        );
        return Err(ApiError::conflict(
            "version_exists",
            format!("version {} already exists and is immutable", body.version),
        ));
    }

    let current: Option<String> = tx
        .query_row(
            "SELECT latest_version FROM agents WHERE slug = ?1",
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
            "UPDATE agents SET latest_version = ?2, name = ?3, description = ?4 WHERE slug = ?1",
            params![slug, body.version, name, description],
        )
        .map_err(ApiError::db)?;
    }
    tx.execute(
        "UPDATE agents SET withdrawn_at = NULL, withdrawn_by = NULL
         WHERE slug = ?1 AND withdrawn_at IS NOT NULL",
        params![slug],
    )
    .map_err(ApiError::db)?;

    audit_kind(
        &tx,
        "agent",
        &actor,
        "publish",
        Some(&slug),
        Some(&body.version),
        Some(&format!(
            "{} files, {} bytes, {} skills, visibility={}",
            checked.file_count, checked.size_bytes, checked.skill_count, body.visibility
        )),
    )
    .map_err(ApiError::db)?;
    tx.commit().map_err(ApiError::db)?;

    info!(
        tenant = %tenant.0,
        slug = %slug,
        version = %body.version,
        visibility = %body.visibility,
        files = checked.file_count,
        bytes = checked.size_bytes,
        skills = checked.skill_count,
        latest = advance,
        "agent published"
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "slug": slug,
            "version": body.version,
            "sha256": checked.sha256,
            "size_bytes": checked.size_bytes,
            "file_count": checked.file_count,
            "skill_count": checked.skill_count,
            "latest_version": if advance { json!(body.version) } else { json!(current) },
            "bundle_url": format!(
                "{}/api/v1/agents/{}/{}/bundle",
                app.cfg.public_url, slug, body.version
            ),
        })),
    ))
}

// -------------------------------------------------------------- withdraw ---

#[derive(Debug, Deserialize)]
pub struct WithdrawRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn withdraw(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
    body: Option<Json<WithdrawRequest>>,
) -> ApiResult<Json<Value>> {
    let reason = body.and_then(|Json(body)| body.reason);
    let db = app.db();
    let (owner, takedown_at): (String, Option<i64>) = db
        .query_row(
            "SELECT owner_tenant, takedown_at FROM agents WHERE slug = ?1",
            params![slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| ApiError::not_found("unknown agent"))?;
    if owner != tenant.0 {
        return Err(ApiError::forbidden("only the owning tenant may withdraw"));
    }
    if takedown_at.is_some() {
        return Err(ApiError::conflict(
            "taken_down",
            "already taken down by an administrator",
        ));
    }
    db.execute(
        "UPDATE agents SET withdrawn_at = ?2, withdrawn_by = ?3 WHERE slug = ?1",
        params![slug, now(), tenant.0],
    )
    .map_err(ApiError::db)?;
    audit_kind(
        &db,
        "agent",
        &tenant.actor(),
        "withdraw",
        Some(&slug),
        None,
        reason.as_deref(),
    )
    .map_err(ApiError::db)?;
    info!(tenant = %tenant.0, slug = %slug, "agent withdrawn");
    Ok(Json(json!({ "slug": slug, "state": "withdrawn" })))
}

pub async fn restore(
    State(app): State<Shared>,
    tenant: TenantId,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let db = app.db();
    let (owner, takedown_at): (String, Option<i64>) = db
        .query_row(
            "SELECT owner_tenant, takedown_at FROM agents WHERE slug = ?1",
            params![slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| ApiError::not_found("unknown agent"))?;
    if owner != tenant.0 {
        return Err(ApiError::forbidden("only the owning tenant may restore"));
    }
    if takedown_at.is_some() {
        return Err(ApiError::conflict(
            "taken_down",
            "an administrator took this agent down; it cannot be self-restored",
        ));
    }
    db.execute(
        "UPDATE agents SET withdrawn_at = NULL, withdrawn_by = NULL WHERE slug = ?1",
        params![slug],
    )
    .map_err(ApiError::db)?;
    audit_kind(
        &db,
        "agent",
        &tenant.actor(),
        "restore",
        Some(&slug),
        None,
        None,
    )
    .map_err(ApiError::db)?;
    info!(tenant = %tenant.0, slug = %slug, "agent restored");
    Ok(Json(json!({ "slug": slug, "state": "active" })))
}

// ----------------------------------------------------------------- bundle ---

/// GET /api/v1/agents/{slug}/{version}/bundle
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
             FROM agents WHERE slug = ?1",
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
        .map_err(|_| ApiError::new(StatusCode::GONE, "gone", "unknown agent"))?;

    if takedown_at.is_some() {
        return Err(ApiError::new(
            StatusCode::GONE,
            "taken_down",
            "this agent was taken down by an administrator and can no longer be downloaded",
        ));
    }
    let is_owner = owner == tenant.0;
    if withdrawn_at.is_some() && !is_owner {
        return Err(ApiError::new(
            StatusCode::GONE,
            "withdrawn",
            "the publisher withdrew this agent",
        ));
    }
    if visibility != "public" && !is_owner {
        return Err(ApiError::forbidden("this agent is private to its tenant"));
    }

    let resolved = if version == "latest" {
        latest.ok_or_else(|| ApiError::new(StatusCode::GONE, "gone", "no published version"))?
    } else {
        version
    };

    let (name, description, bundle_json): (String, String, String) = db
        .query_row(
            "SELECT a.name, a.description, v.bundle_json FROM agents a
             JOIN agent_versions v ON v.slug = a.slug
             WHERE a.slug = ?1 AND v.version = ?2",
            params![slug, resolved],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| ApiError::not_found("unknown version"))?;

    let files: BTreeMap<String, FileData> = serde_json::from_str(&bundle_json).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt_bundle",
            format!("stored agent bundle is unreadable: {err}"),
        )
    })?;

    debug!(
        tenant = %tenant.0,
        slug = %slug,
        version = %resolved,
        files = files.len(),
        "agent bundle fetched"
    );
    Ok(Json(json!({
        "name": name,
        "description": description,
        "version": resolved,
        "files": files,
        "source_url": format!(
            "{}/api/v1/agents/{}/{}/bundle",
            app.cfg.public_url, slug, resolved
        ),
    })))
}

// ------------------------------------------------------------------ admin ---

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

/// GET /admin/api/agents
pub async fn admin_list(
    State(app): State<Shared>,
    _admin: Admin,
    Query(query): Query<AdminListQuery>,
) -> Result<Json<Value>, ApiError> {
    let pattern = format!("%{}%", query.q.trim());
    let limit = query.limit.clamp(1, 200);
    let db = app.db();
    let sql = format!(
        "{AGENT_SELECT}
         WHERE a.slug LIKE ?1 OR a.name LIKE ?1
         ORDER BY a.created_at DESC
         LIMIT ?2"
    );
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let items = stmt
        .query_map(params![pattern, limit], |row| {
            row_to_agent(row, &app.cfg.public_url)
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;
    debug!(items = items.len(), "admin agent listing");
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
pub struct TakedownRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// POST /admin/api/agents/{slug}/takedown
pub async fn admin_takedown(
    State(app): State<Shared>,
    admin: Admin,
    Path(slug): Path<String>,
    body: Option<Json<TakedownRequest>>,
) -> Result<Json<Value>, ApiError> {
    let reason = body
        .and_then(|Json(body)| body.reason)
        .filter(|reason| !reason.trim().is_empty());
    let db = app.db();
    let changed = db
        .execute(
            "UPDATE agents SET takedown_at = ?2, takedown_by = ?3, takedown_reason = ?4
             WHERE slug = ?1",
            params![slug, now(), admin.0, reason],
        )
        .map_err(ApiError::db)?;
    if changed == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "unknown agent",
        ));
    }
    audit_kind(
        &db,
        "agent",
        &admin.actor(),
        "takedown",
        Some(&slug),
        None,
        reason.as_deref(),
    )
    .map_err(ApiError::db)?;
    info!(
        admin = %admin.0,
        slug = %slug,
        reason = reason.as_deref().unwrap_or("-"),
        "agent taken down"
    );
    Ok(Json(json!({ "slug": slug, "state": "taken_down" })))
}

/// POST /admin/api/agents/{slug}/restore
pub async fn admin_restore(
    State(app): State<Shared>,
    admin: Admin,
    Path(slug): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let db = app.db();
    let changed = db
        .execute(
            "UPDATE agents SET takedown_at = NULL, takedown_by = NULL, takedown_reason = NULL
             WHERE slug = ?1",
            params![slug],
        )
        .map_err(ApiError::db)?;
    if changed == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "unknown agent",
        ));
    }
    audit_kind(
        &db,
        "agent",
        &admin.actor(),
        "admin_restore",
        Some(&slug),
        None,
        None,
    )
    .map_err(ApiError::db)?;
    info!(admin = %admin.0, slug = %slug, "agent takedown revoked");
    Ok(Json(json!({ "slug": slug, "state": "active" })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(manifest: Value, extra: &[(&str, &str)]) -> BTreeMap<String, FileData> {
        let mut files = BTreeMap::new();
        files.insert(
            AGENT_MANIFEST.to_string(),
            FileData::from(manifest.to_string()),
        );
        for (path, content) in extra {
            files.insert((*path).to_string(), FileData::from(*content));
        }
        files
    }

    fn manifest() -> Value {
        json!({
            "schema": 1,
            "name": "QA Agent",
            "description": "demo",
            "language": "zh",
            "profile": { "thinking_level": "medium", "plan_enabled": false,
                         "disabled_tools": ["browser"] },
            "md_files": ["SOUL.md"],
            "skills": ["demo-skill"]
        })
    }

    const SKILL: &str = "---\nname: demo-skill\ndescription: a demo\n---\n";

    #[test]
    fn accepts_a_well_formed_bundle() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("skills/demo-skill/SKILL.md", SKILL),
                ("skills/demo-skill/scripts/run.py", "print(1)\n"),
            ],
        );
        let checked = check_agent_bundle(&files).expect("valid bundle");
        assert_eq!(checked.manifest.name, "QA Agent");
        assert_eq!(checked.skill_count, 1);
        assert_eq!(checked.file_count, 4);
    }

    #[test]
    fn rejects_missing_manifest() {
        let files = BTreeMap::from([("SOUL.md".to_string(), FileData::from("# soul\n"))]);
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "missing_manifest"
        );
    }

    #[test]
    fn rejects_undeclared_skill_files() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("skills/demo-skill/SKILL.md", SKILL),
                ("skills/other/run.py", "print(1)\n"),
            ],
        );
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "undeclared_skill"
        );
    }

    #[test]
    fn rejects_unexpected_root_files() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("MEMORY.md", "private\n"),
                ("skills/demo-skill/SKILL.md", SKILL),
            ],
        );
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "unexpected_root_file"
        );
    }

    #[test]
    fn rejects_missing_skill_frontmatter() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("skills/demo-skill/SKILL.md", "# no frontmatter\n"),
            ],
        );
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "invalid_manifest"
        );
    }

    #[test]
    fn rejects_path_traversal() {
        let files = bundle(
            manifest(),
            &[
                ("SOUL.md", "# soul\n"),
                ("skills/demo-skill/SKILL.md", SKILL),
                ("skills/demo-skill/../escape.py", "print(1)\n"),
            ],
        );
        assert_eq!(check_agent_bundle(&files).unwrap_err().code, "traversal");
    }

    #[test]
    fn rejects_bad_thinking_budget() {
        let mut manifest = manifest();
        manifest["profile"] = json!({ "thinking_level": "medium", "thinking_budget": 100 });
        let files = bundle(manifest, &[("SOUL.md", "# soul\n")]);
        assert_eq!(
            check_agent_bundle(&files).unwrap_err().code,
            "invalid_profile"
        );
    }
}
