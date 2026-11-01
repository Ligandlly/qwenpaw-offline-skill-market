//! What a write would be refused for — one vocabulary shared by the dry run
//! (which reports every problem) and the publish itself (which refuses on the
//! first one).

use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use std::collections::BTreeMap;

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::limits::{self, FileData};
use crate::state::Shared;
use tracing::debug;

use super::Entity;

/// One reason a write would be refused. The dry run reports all of them; the
/// publish itself refuses on the first one — same code, same wording.
pub(crate) struct Problem {
    pub code: &'static str,
    pub message: String,
}

impl Problem {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn json(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }
}

impl From<limits::Reject> for Problem {
    fn from(reject: limits::Reject) -> Self {
        Self {
            code: reject.code,
            message: reject.message,
        }
    }
}

impl From<Problem> for ApiError {
    fn from(problem: Problem) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, problem.code, &problem.message)
    }
}

/// Field-level rules both `validate` endpoints apply before they look at the
/// bundle itself.
pub(crate) fn check_fields(version: &str, visibility: &str, problems: &mut Vec<Problem>) {
    if !matches!(visibility, "public" | "tenant") {
        problems.push(Problem::new(
            "invalid_visibility",
            "可见范围只能是 public 或 tenant",
        ));
    }
    if !limits::valid_version(version) {
        problems.push(Problem::new(
            "invalid_version",
            "版本号需为 1-32 位，只能包含 0-9 A-Z a-z . + -，且以字母或数字开头",
        ));
    }
}

/// The slug a publish would land on: the explicit one, else the plane's own
/// derivation from the display name.
pub(crate) fn resolve_slug(
    entity: &Entity,
    app: &Shared,
    tenant: &str,
    name: &str,
    explicit: Option<&str>,
    problems: &mut Vec<Problem>,
) -> Option<String> {
    let slug = match explicit.map(str::trim).filter(|slug| !slug.is_empty()) {
        Some(slug) => Some(slug.to_string()),
        None => (entity.slug_from)(app, tenant, name),
    };
    match &slug {
        None => problems.push(Problem::new(
            "invalid_slug",
            "无法从名称推导出标识符，请手动填写一个只含小写字母、数字和 - 的标识符",
        )),
        Some(slug) if !limits::valid_slug(slug) => problems.push(Problem::new(
            "invalid_slug",
            "标识符需为 1-64 位，只能包含小写字母、数字和 -，且以字母或数字开头",
        )),
        Some(_) => {}
    }
    slug
}

struct Existing {
    name: String,
    owner_tenant: String,
    latest_version: Option<String>,
    takedown_at: Option<i64>,
    withdrawn_at: Option<i64>,
}

/// Entry-level rules for an existing slug: ownership, takedown and version
/// uniqueness — the same questions the publish path answers with an error.
///
/// Returns the `existing` block and the suggested next version, both of which
/// the publish page shows instead of making the user guess.
/// The slug a publish lands on, refusing with the first problem instead of
/// collecting them: a publish either happens or does not.
pub(crate) fn required_slug(
    entity: &Entity,
    app: &Shared,
    tenant: &str,
    name: &str,
    explicit: Option<&str>,
) -> Result<String, Problem> {
    let mut problems: Vec<Problem> = Vec::new();
    let slug = resolve_slug(entity, app, tenant, name, explicit, &mut problems);
    match (problems.into_iter().next(), slug) {
        (Some(problem), _) => Err(problem),
        (None, Some(slug)) => Ok(slug),
        (None, None) => Err(Problem::new(
            "invalid_slug",
            "无法从名称推导出标识符，请手动填写一个只含小写字母、数字和 - 的标识符",
        )),
    }
}

/// The field rules of a publish: exactly what the dry run would report.
pub(crate) fn require_fields(version: &str, visibility: &str) -> ApiResult<()> {
    let mut problems: Vec<Problem> = Vec::new();
    check_fields(version, visibility, &mut problems);
    match problems.into_iter().next() {
        Some(problem) => Err(problem.into()),
        None => Ok(()),
    }
}

pub(crate) fn inspect(
    db: &Connection,
    entity: &Entity,
    tenant: &str,
    slug: &str,
    version: &str,
    problems: &mut Vec<Problem>,
) -> ApiResult<(Option<Value>, Option<String>)> {
    let label = entity.label;
    let row: Option<Existing> = db
        .query_row(
            &format!(
                "SELECT name, owner_tenant, latest_version, takedown_at, withdrawn_at
                 FROM {} WHERE slug = ?1",
                entity.table
            ),
            params![slug],
            |row| {
                Ok(Existing {
                    name: row.get(0)?,
                    owner_tenant: row.get(1)?,
                    latest_version: row.get(2)?,
                    takedown_at: row.get(3)?,
                    withdrawn_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(ApiError::db)?;

    let mut next_version = None;
    let existing = row.map(|row| {
        let mine = row.owner_tenant == tenant;
        if mine {
            next_version = row
                .latest_version
                .as_deref()
                .and_then(crate::version::suggest_next_version);
            if row.takedown_at.is_some() {
                problems.push(Problem::new(
                    "taken_down",
                    format!("该{label}已被管理员下架，不能再发布"),
                ));
            }
        } else {
            problems.push(Problem::new(
                "slug_taken",
                format!("标识符 '{slug}' 已被其他团队占用"),
            ));
        }
        json!({
            "name": row.name,
            "owner_tenant": row.owner_tenant,
            "latest_version": row.latest_version,
            "state": if row.takedown_at.is_some() {
                "taken_down"
            } else if row.withdrawn_at.is_some() {
                "withdrawn"
            } else {
                "active"
            },
            "mine": mine,
        })
    });

    let taken: i64 = db
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM {} WHERE slug = ?1 AND version = ?2",
                entity.version_table
            ),
            params![slug, version],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if taken > 0 {
        problems.push(Problem::new(
            "version_exists",
            format!("版本 {version} 已存在且不可修改，请不要重复发布同一个版本号"),
        ));
    }
    Ok((existing, next_version))
}

/// The body a `validate` endpoint returns. Identical on both planes: only the
/// manifest preview and the problems themselves differ.
pub(crate) struct Answer<'a> {
    pub cfg: &'a Config,
    pub name: String,
    pub version: String,
    pub visibility: String,
    pub slug: Option<String>,
    pub files: &'a BTreeMap<String, FileData>,
    pub manifest: Option<Value>,
    pub existing: Option<Value>,
    pub next_version: Option<String>,
    pub problems: Vec<Problem>,
}

impl Answer<'_> {
    pub fn to_json(&self) -> Value {
        let size_bytes: usize = self.files.values().map(FileData::len).sum();
        let listing: Vec<Value> = self
            .files
            .iter()
            .map(|(path, content)| json!({ "path": path, "size_bytes": content.len() }))
            .collect();
        json!({
            "ok": self.problems.is_empty(),
            "slug": self.slug,
            "name": self.name.trim(),
            "version": self.version,
            "visibility": self.visibility,
            "file_count": self.files.len(),
            "size_bytes": size_bytes,
            "files": listing,
            "manifest": self.manifest,
            "existing": self.existing,
            "next_version": self.next_version,
            "limits": {
                "max_files": self.cfg.limits.max_files,
                "max_file_bytes": self.cfg.limits.max_file_bytes,
                "max_bundle_bytes": self.cfg.limits.max_bundle_bytes,
            },
            "problems": self.problems.iter().map(Problem::json).collect::<Vec<_>>(),
        })
    }
}

/// `GET /api/v1/{skills,agents}/lookup?names=a,b`: display name → slug + state.
///
/// Unlike a listing this includes withdrawn and taken-down entries, and it
/// ignores the public filter for the caller's own entries: the host uses it to
/// map an installed item back to the market, so "it is gone" is an answer, not
/// a reason to pretend the item never existed.
pub(crate) fn lookup(
    db: &Connection,
    entity: &Entity,
    tenant: &str,
    names: &str,
    public_url: &str,
) -> ApiResult<Value> {
    let wanted: Vec<&str> = names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .take(100)
        .collect();
    if wanted.is_empty() {
        return Ok(json!({ "items": [] }));
    }

    let placeholders = (0..wanted.len())
        .map(|index| format!("?{}", index + 2))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT slug, name, owner_tenant, visibility, latest_version,
                withdrawn_at, takedown_at, takedown_reason
         FROM {}
         WHERE name IN ({placeholders}) AND (visibility = 'public' OR owner_tenant = ?1)",
        entity.table
    );

    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let mut bindings: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(tenant.to_string())];
    for name in &wanted {
        bindings.push(Box::new((*name).to_string()));
    }
    let refs: Vec<&dyn rusqlite::ToSql> = bindings.iter().map(|binding| binding.as_ref()).collect();
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
            let slug: String = row.get("slug")?;
            let latest: Option<String> = row.get("latest_version")?;
            let mut item = json!({
                "slug": slug,
                "name": row.get::<_, String>("name")?,
                "owner_tenant": row.get::<_, String>("owner_tenant")?,
                "visibility": row.get::<_, String>("visibility")?,
                "latest_version": latest,
                "state": state,
                "takedown_reason": row.get::<_, Option<String>>("takedown_reason")?,
            });
            let url = latest
                .as_ref()
                .map(|version| (entity.bundle_url)(public_url, &slug, version))
                .unwrap_or_default();
            item[entity.bundle_field] = json!(url);
            Ok(item)
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;

    debug!(tenant = %tenant, wanted = wanted.len(), found = items.len(), "lookup");
    Ok(json!({ "items": items }))
}
