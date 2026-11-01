//! What the market shows: the paged list, the owner's own list, the admin list,
//! one entry's state, and a version's bytes.

use std::collections::BTreeMap;

use axum::http::StatusCode;

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::config::Config;
use crate::db::now;
use crate::dto::{page_offset, ListQuery};
use crate::error::{ApiError, ApiResult};
use crate::limits::FileData;

use super::entity::{normalize_sort, row_to_entry, Entry, Entity, Labels};

/// Records that a tenant is alive. The market listing is the only place that
/// observes traffic, so it doubles as the "last seen" beacon.
pub(crate) fn touch_tenant(db: &Connection, tenant: &str) -> ApiResult<()> {
    db.execute(
        "INSERT INTO tenants (id, first_seen, last_seen) VALUES (?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen",
        params![tenant, now()],
    )
    .map_err(ApiError::db)?;
    Ok(())
}

/// `GET /api/v1/{skills,agents}`: one page of the market.
pub(crate) fn list_page(
    db: &Connection,
    entity: &Entity,
    tenant: &str,
    query: &ListQuery,
    public_url: &str,
) -> ApiResult<Value> {
    let limit = query.limit.clamp(1, 50);
    let offset = page_offset(query.page, limit)?;
    let pattern = format!("%{}%", query.q.trim());
    let order = (entity.order)(&query.sort);
    let alias = entity.alias;

    touch_tenant(db, tenant)?;

    let filter = format!(
        "{alias}.takedown_at IS NULL AND {alias}.withdrawn_at IS NULL
           AND ({alias}.visibility = 'public' OR {alias}.owner_tenant = ?1)
           AND ({alias}.slug LIKE ?2 OR {alias}.name LIKE ?2)"
    );
    let total: i64 = db
        .query_row(
            &format!("SELECT COUNT(*) FROM {} {alias} WHERE {filter}", entity.table),
            params![tenant, pattern],
            |row| row.get(0),
        )
        .map_err(ApiError::db)?;

    let sql = format!(
        "{}\n WHERE {filter}\n ORDER BY {order}\n LIMIT ?3 OFFSET ?4",
        entity.select
    );
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let items = stmt
        .query_map(params![tenant, pattern, limit, offset], |row| {
            row_to_entry(row, entity, public_url)
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;

    debug!(
        tenant = %tenant,
        q = %query.q,
        page = query.page,
        limit,
        items = items.len(),
        "market listing"
    );
    Ok(json!({
        "page": query.page,
        "limit": limit,
        "total": total,
        "sort": normalize_sort(&query.sort),
        "items": entries_to_json(entity, &items),
    }))
}

/// `GET /api/v1/{skills,agents}/mine`: everything the tenant owns, withdrawn
/// entries included, newest first.
pub(crate) fn list_owned(
    db: &Connection,
    entity: &Entity,
    tenant: &str,
    public_url: &str,
) -> ApiResult<Value> {
    let alias = entity.alias;
    let sql = format!(
        "{} WHERE {alias}.owner_tenant = ?1 ORDER BY {alias}.created_at DESC",
        entity.select
    );
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let items = stmt
        .query_map(params![tenant], |row| row_to_entry(row, entity, public_url))
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;
    debug!(tenant = %tenant, items = items.len(), "own entries");
    Ok(json!({ "items": entries_to_json(entity, &items) }))
}

/// `GET /admin/api/{skills,agents}`: the unfiltered operator view.
pub(crate) fn list_admin(
    db: &Connection,
    entity: &Entity,
    q: &str,
    limit: i64,
    public_url: &str,
) -> ApiResult<Value> {
    let alias = entity.alias;
    let pattern = format!("%{}%", q.trim());
    let sql = format!(
        "{} WHERE {alias}.slug LIKE ?1 OR {alias}.name LIKE ?1
         ORDER BY {alias}.created_at DESC LIMIT ?2",
        entity.select
    );
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let items = stmt
        .query_map(params![pattern, limit], |row| {
            row_to_entry(row, entity, public_url)
        })
        .map_err(ApiError::db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(ApiError::db)?;
    debug!(q = %q, items = items.len(), "admin listing");
    Ok(json!({ "items": entries_to_json(entity, &items) }))
}

fn entries_to_json(entity: &Entity, entries: &[Entry]) -> Vec<Value> {
    entries.iter().map(|entry| entry.to_json(entity)).collect()
}

/// `GET /api/v1/{skills,agents}/{slug}/status`: the owner's or the public's
/// view of one entry.
///
/// Unlike [`bundle`] an entry that is not visible at all is a plain 404: this
/// endpoint is a status lookup, not a probe for somebody else's slug.
pub(crate) fn status(
    db: &Connection,
    entity: &Entity,
    tenant: &str,
    slug: &str,
) -> ApiResult<Value> {
    let missing = || ApiError::new(StatusCode::NOT_FOUND, "not_found", format!("{}不存在", entity.label));
    let sql = format!(
        "SELECT slug, name, owner_tenant, visibility, latest_version, withdrawn_at,
                takedown_at, takedown_reason
         FROM {} WHERE slug = ?1",
        entity.table
    );
    let mut stmt = db.prepare(&sql).map_err(ApiError::db)?;
    let mut rows = stmt.query(params![slug]).map_err(ApiError::db)?;
    let Some(row) = rows.next().map_err(ApiError::db)? else {
        debug!(tenant = %tenant, slug = %slug, "status: unknown entry");
        return Err(missing());
    };
    let owner: String = row.get("owner_tenant").map_err(ApiError::db)?;
    let visibility: String = row.get("visibility").map_err(ApiError::db)?;
    if owner != tenant && visibility != "public" {
        debug!(tenant = %tenant, slug = %slug, owner = %owner, visibility = %visibility,
               "status: not visible to tenant");
        return Err(missing());
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
    debug!(tenant = %tenant, slug = %slug, state = %state, "status");
    Ok(json!({
        "slug": row.get::<_, String>("slug").map_err(ApiError::db)?,
        "name": row.get::<_, String>("name").map_err(ApiError::db)?,
        "latest_version": row.get::<_, Option<String>>("latest_version").map_err(ApiError::db)?,
        "state": state,
        "takedown_reason": row.get::<_, Option<String>>("takedown_reason").map_err(ApiError::db)?,
    }))
}

/// A version handed back to a host, already resolved and counted.
///
/// The planes differ in one field (`description`, agent only) and in the URL
/// they advertise, so each handler assembles its own JSON from this.
pub(crate) struct BundlePayload {
    pub name: String,
    pub description: Option<String>,
    pub version: String,
    pub files: BTreeMap<String, FileData>,
    pub source_url: String,
}

/// `GET …/{slug}/{version}/bundle`: the download path, shared end to end.
///
/// Precedence is part of the contract: taken down beats withdrawn (also for
/// the owner), withdrawn and non-public only hide an entry from other tenants,
/// and an unknown slug is 410 rather than 404 so a removed entry reads the
/// same as one that never existed.
pub(crate) fn bundle(
    db: &Connection,
    entity: &Entity,
    cfg: &Config,
    tenant: &str,
    slug: &str,
    version: &str,
) -> ApiResult<BundlePayload> {
    let label = entity.label;
    let (owner, visibility, latest, takedown_at, withdrawn_at, row_name): (
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
        String,
    ) = db
        .query_row(
            &format!(
                "SELECT owner_tenant, visibility, latest_version, takedown_at, withdrawn_at, name
                 FROM {} WHERE slug = ?1",
                entity.table
            ),
            params![slug],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map_err(|err| ApiError::query_gone(err, &format!("{label}不存在")))?;

    if takedown_at.is_some() {
        debug!(tenant = %tenant, slug = %slug, "bundle fetch: taken down");
        return Err(ApiError::new(
            StatusCode::GONE,
            "taken_down",
            format!("该{label}已被管理员下架，无法再下载"),
        ));
    }
    let is_owner = owner == tenant;
    if withdrawn_at.is_some() && !is_owner {
        debug!(tenant = %tenant, slug = %slug, "bundle fetch: withdrawn");
        return Err(ApiError::new(
            StatusCode::GONE,
            "withdrawn",
            format!("发布者已撤回该{label}"),
        ));
    }
    if visibility != "public" && !is_owner {
        debug!(tenant = %tenant, slug = %slug, visibility = %visibility,
               "bundle fetch: not public");
        return Err(ApiError::forbidden(format!("该{label}仅对所属团队可见")));
    }

    let resolved = if version == "latest" {
        latest.ok_or_else(|| {
            ApiError::new(StatusCode::GONE, "gone", "还没有已发布的版本")
        })?
    } else {
        version.to_string()
    };

    let bundle_json: String = db
        .query_row(
            &format!(
                "SELECT bundle_json FROM {} WHERE slug = ?1 AND version = ?2",
                entity.version_table
            ),
            params![slug, resolved],
            |row| row.get(0),
        )
        .map_err(|err| ApiError::query_miss(err, "版本不存在"))?;

    let files: BTreeMap<String, FileData> = serde_json::from_str(&bundle_json).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt_bundle",
            format!("存储的包无法读取：{err}"),
        )
    })?;

    let (name, description) = labels_of(entity, &files, &row_name)?;

    if let Err(err) = db.execute(
        &format!("UPDATE {} SET download_count = download_count + 1 WHERE slug = ?1", entity.table),
        params![slug],
    ) {
        // A counter must never fail an install that already succeeded.
        warn!(slug = %slug, error = %err, "download counter update failed");
    }

    debug!(
        tenant = %tenant,
        slug = %slug,
        version = %resolved,
        files = files.len(),
        bytes = files.values().map(FileData::len).sum::<usize>(),
        "bundle fetched"
    );
    Ok(BundlePayload {
        name,
        description,
        source_url: (entity.bundle_url)(&cfg.public_url, slug, &resolved),
        version: resolved,
        files,
    })
}

/// Resolves a payload's labels from the source the plane declares.
fn labels_of(
    entity: &Entity,
    files: &BTreeMap<String, FileData>,
    row_name: &str,
) -> ApiResult<(String, Option<String>)> {
    match entity.bundle_labels {
        Labels::Column => Ok((row_name.to_string(), None)),
        Labels::File(path) => {
            let corrupt = || {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "corrupt_bundle",
                    format!("存储的{}包缺少可读的 {path}", entity.label),
                )
            };
            let text = files.get(path).and_then(FileData::as_text).ok_or_else(corrupt)?;
            let manifest: Value = serde_json::from_str(text).map_err(|_| corrupt())?;
            let text_of = |key: &str| -> Option<String> {
                manifest
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            };
            let name = text_of("name").ok_or_else(corrupt)?;
            Ok((name, text_of("description")))
        }
        Labels::ManifestColumn => Ok((row_name.to_string(), None)),
    }
}

// ---------------------------------------------------------------- writing ---

