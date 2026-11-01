//! What changes an entry: publishing an immutable version (and moving the
//! `latest` pointer), and the four state changes. Each one writes its audit
//! entry in the same transaction as the change itself.

use rusqlite::{named_params, params, Connection, OptionalExtension};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::db::{audit_kind, now};
use crate::error::{ApiError, ApiResult};

use super::Entity;

/// Records that this tenant published: `display_name` is only written when the
/// plugin actually sent one, so a publish without it cannot wipe the name an
/// earlier one recorded.
pub(crate) fn record_tenant(db: &Connection, tenant: &str, display_name: Option<&str>) -> ApiResult<()> {
    db.execute(
        "INSERT INTO tenants (id, first_seen, last_seen, display_name) VALUES (?1, ?2, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen,
             display_name = COALESCE(excluded.display_name, tenants.display_name)",
        params![tenant, now(), display_name],
    )
    .map_err(ApiError::db)?;
    Ok(())
}

/// The listing row a write is about to touch.
pub(crate) struct Listing {
    pub owner_tenant: String,
    pub latest_version: Option<String>,
    pub taken_down: bool,
}

impl Listing {
    /// Only the publishing tenant may write, and nobody may write while an
    /// admin's takedown stands. The messages differ per plane, so they come in
    /// from the handler.
    pub fn guard(&self, tenant: &str, forbidden: &str, taken_down: &str) -> ApiResult<()> {
        if self.owner_tenant != tenant {
            warn!(
                tenant = %tenant,
                owner = %self.owner_tenant,
                "write rejected: not the owner"
            );
            return Err(ApiError::forbidden(forbidden));
        }
        if self.taken_down {
            warn!(tenant = %tenant, "write rejected: taken down");
            return Err(ApiError::conflict("taken_down", taken_down));
        }
        Ok(())
    }
}

/// Loads the listing a write targets. `None` means the slug is free — a real
/// read failure is an error, never "not there".
pub(crate) fn listing(db: &Connection, entity: &Entity, slug: &str) -> ApiResult<Option<Listing>> {
    db.query_row(
        entity.select_lifecycle,
        params![slug],
        |row| {
            Ok(Listing {
                owner_tenant: row.get(0)?,
                latest_version: row.get(1)?,
                taken_down: row.get::<_, Option<i64>>(2)?.is_some(),
            })
        },
    )
    .optional()
    .map_err(ApiError::db)
}

/// A publish, flattened: what both gates hand back plus what the handler knows.
pub(crate) struct Publish<'a> {
    pub slug: &'a str,
    pub tenant: &'a str,
    pub version: &'a str,
    pub visibility: &'a str,
    pub name: &'a str,
    /// Only the agent plane stores a description column; the skill plane reads
    /// it out of the bundle on demand.
    pub description: Option<&'a str>,
    pub file_count: usize,
    /// Only the agent plane counts the skills a bundle ships.
    pub skill_count: Option<i64>,
    pub size_bytes: usize,
    pub sha256: &'a str,
    pub bundle_json: String,
    /// The `latest_version` before this publish, if the entry existed.
    pub previous: Option<String>,
}

/// What the publish response needs to know about the `latest` pointer.
pub(crate) struct Advances {
    pub advanced: bool,
    pub latest_version: Value,
}

/// Writes one immutable version and moves the `latest` pointer, in a single
/// transaction with its audit entry.
pub(crate) fn store(
    db: &Connection,
    entity: &Entity,
    publish: Publish<'_>,
    detail: &str,
    actor: &str,
) -> ApiResult<Advances> {
    let Publish {
        slug,
        tenant,
        version,
        visibility,
        name,
        description,
        file_count,
        skill_count,
        size_bytes,
        sha256,
        bundle_json,
        previous,
    } = publish;
    let tx = db.unchecked_transaction().map_err(ApiError::db)?;
    // rusqlite refuses a named parameter the statement does not declare, so the
    // optional columns are bound only when this plane actually has them.
    match description {
        Some(description) => tx.execute(
            entity.insert_listing,
            named_params! {
                ":slug": slug,
                ":name": name,
                ":description": description,
                ":owner": tenant,
                ":visibility": visibility,
                ":version": version,
                ":now": now(),
            },
        ),
        None => tx.execute(
            entity.insert_listing,
            named_params! {
                ":slug": slug,
                ":name": name,
                ":owner": tenant,
                ":visibility": visibility,
                ":version": version,
                ":now": now(),
            },
        ),
    }
    .map_err(ApiError::db)?;

    let inserted = match skill_count {
        Some(skill_count) => tx.execute(
            entity.insert_version,
            named_params! {
                ":slug": slug,
                ":version": version,
                ":sha256": sha256,
                ":size_bytes": size_bytes as i64,
                ":file_count": file_count as i64,
                ":skill_count": skill_count,
                ":bundle_json": bundle_json,
                ":now": now(),
            },
        ),
        None => tx.execute(
            entity.insert_version,
            named_params! {
                ":slug": slug,
                ":version": version,
                ":sha256": sha256,
                ":size_bytes": size_bytes as i64,
                ":file_count": file_count as i64,
                ":bundle_json": bundle_json,
                ":now": now(),
            },
        ),
    }
    .map_err(ApiError::db)?;
    if inserted == 0 {
        warn!(slug = %slug, version = %version, "publish rejected: version already exists");
        return Err(ApiError::conflict(
            "version_exists",
            format!("版本 {version} 已存在且不可修改，请不要重复发布同一个版本号"),
        ));
    }

    // The pointer only moves forward: a late publish of an old version must not
    // roll the market back. The name travels with the pointer, so re-publishing
    // an older version cannot rename the listing backwards.
    let advanced = previous
        .as_deref()
        .map(|cur| crate::version::compare_versions(version, cur).is_gt())
        .unwrap_or(true);
    if advanced {
        tx.execute(
            &format!(
                "UPDATE {} SET latest_version = ?2, name = ?3 WHERE slug = ?1",
                entity.table
            ),
            params![slug, version, name],
        )
        .map_err(ApiError::db)?;
        if let Some(description) = description {
            tx.execute(
                &format!("UPDATE {} SET description = ?2 WHERE slug = ?1", entity.table),
                params![slug, description],
            )
            .map_err(ApiError::db)?;
        }
    }
    // Publishing again is also how an owner un-withdraws: the new version is
    // the one the market should serve.
    tx.execute(
        &format!(
            "UPDATE {} SET withdrawn_at = NULL, withdrawn_by = NULL
             WHERE slug = ?1 AND withdrawn_at IS NOT NULL",
            entity.table
        ),
        params![slug],
    )
    .map_err(ApiError::db)?;

    audit_kind(
        &tx,
        entity.kind,
        actor,
        "publish",
        Some(slug),
        Some(version),
        Some(detail),
    )
    .map_err(ApiError::db)?;
    tx.commit().map_err(ApiError::db)?;

    Ok(Advances {
        advanced,
        latest_version: if advanced {
            json!(version)
        } else {
            json!(previous)
        },
    })
}

/// One state change on a listing, whichever plane and whoever asks for it.
pub(crate) enum Change {
    /// The owner takes its own entry off the market.
    Withdraw { reason: Option<String> },
    /// The owner puts it back.
    Restore,
    /// An admin takes it off the market for everyone.
    Takedown { reason: Option<String> },
    /// An admin revokes a takedown.
    Lift,
}

impl Change {
    fn action(&self) -> &'static str {
        match self {
            Change::Withdraw { .. } => "withdraw",
            Change::Restore => "restore",
            Change::Takedown { .. } => "takedown",
            Change::Lift => "admin_restore",
        }
    }

    fn state(&self) -> &'static str {
        match self {
            Change::Withdraw { .. } => "withdrawn",
            Change::Restore | Change::Lift => "active",
            Change::Takedown { .. } => "taken_down",
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Change::Withdraw { reason } | Change::Takedown { reason } => reason.as_deref(),
            Change::Restore | Change::Lift => None,
        }
    }
}

/// Applies a state change and its audit entry in one transaction: if the trail
/// cannot be written, the change must not stand (`tests/faults.rs` pins this
/// down).
pub(crate) fn change_state(
    db: &Connection,
    entity: &Entity,
    slug: &str,
    change: Change,
    actor: &str,
) -> ApiResult<Value> {
    let tx = db.unchecked_transaction().map_err(ApiError::db)?;
    let changed = match &change {
        Change::Withdraw { .. } => tx.execute(
            &format!(
                "UPDATE {} SET withdrawn_at = ?2, withdrawn_by = ?3 WHERE slug = ?1",
                entity.table
            ),
            params![slug, now(), actor],
        ),
        Change::Restore => tx.execute(
            &format!(
                "UPDATE {} SET withdrawn_at = NULL, withdrawn_by = NULL WHERE slug = ?1",
                entity.table
            ),
            params![slug],
        ),
        Change::Takedown { reason } => tx.execute(
            &format!(
                "UPDATE {} SET takedown_at = ?2, takedown_by = ?3, takedown_reason = ?4
                 WHERE slug = ?1",
                entity.table
            ),
            params![slug, now(), actor, reason],
        ),
        Change::Lift => tx.execute(
            &format!(
                "UPDATE {} SET takedown_at = NULL, takedown_by = NULL, takedown_reason = NULL
                 WHERE slug = ?1",
                entity.table
            ),
            params![slug],
        ),
    }
    .map_err(ApiError::db)?;
    if changed == 0 {
        warn!(actor = %actor, slug = %slug, action = change.action(), "state change: unknown entry");
        return Err(ApiError::not_found(&format!("{}不存在", entity.label)));
    }
    audit_kind(
        &tx,
        entity.kind,
        actor,
        change.action(),
        Some(slug),
        None,
        change.reason(),
    )
    .map_err(ApiError::db)?;
    tx.commit().map_err(ApiError::db)?;

    info!(
        entity = entity.label,
        slug = %slug,
        action = change.action(),
        actor = %actor,
        reason = change.reason().unwrap_or("-"),
        "state changed"
    );
    Ok(json!({ "slug": slug, "state": change.state() }))
}

// ------------------------------------------------------------- validating ---
