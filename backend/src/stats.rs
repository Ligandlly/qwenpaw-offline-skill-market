//! Dashboard aggregates for the tenant-facing overview tab.
//!
//! Counts mirror what the market listing shows this tenant: active public
//! items plus its own active private ones. Takedown/withdrawn rows are
//! excluded on purpose — the dashboard is a showcase, not an archive.
//! `download_count` counts successful bundle fetches, not listing views. The
//! bundle handlers bump it in memory and `crate::counters` writes it back every
//! few seconds, so the dashboard can lag by one interval.

use axum::extract::State;
use axum::Json;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::auth::TenantId;
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::state::Shared;

pub async fn overview(State(app): State<Shared>, tenant: TenantId) -> ApiResult<Json<Value>> {
    crate::db::offload(&app, move |app| dashboard(app, &tenant.0))
        .await
        .map(Json)
}

fn dashboard(app: &Shared, tenant: &str) -> ApiResult<Value> {
    let db = app.db();
    let month_start: i64 = db
        .query_row(
            "SELECT CAST(strftime('%s','now','start of month') AS INTEGER)",
            [],
            |row| row.get(0),
        )
        .map_err(ApiError::db)?;

    let skills = counts(&db, "skills", tenant, month_start).map_err(ApiError::db)?;
    let agents = counts(&db, "agents", tenant, month_start).map_err(ApiError::db)?;
    let downloads =
        skills["downloads"].as_i64().unwrap_or(0) + agents["downloads"].as_i64().unwrap_or(0);
    let new_this_month = skills["new_this_month"].as_i64().unwrap_or(0)
        + agents["new_this_month"].as_i64().unwrap_or(0);

    Ok(json!({
        "skills": skills,
        "agents": agents,
        "totals": {
            "skills": skills["total"],
            "agents": agents["total"],
            "downloads": downloads,
            "new_this_month": new_this_month,
        },
        "month_start": month_start,
        "generated_at": now(),
    }))
}

/// Hub-wide aggregates for *other apps* on the same isolated network
/// (`GET /api/v1/hub/stats`). Deliberately unauthenticated and read-only:
/// unlike the tenant dashboard it needs no `X-Tenant-Id`, and it counts every
/// tenant's items — public and private alike. Same showcase semantics as
/// `overview`: active items only for totals/new-this-month, cumulative
/// downloads including withdrawn/taken-down history.
pub async fn hub_overview(State(app): State<Shared>) -> ApiResult<Json<Value>> {
    crate::db::offload(&app, hub_dashboard).await.map(Json)
}

fn hub_dashboard(app: &Shared) -> ApiResult<Value> {
    let db = app.db();
    let month_start: i64 = db
        .query_row(
            "SELECT CAST(strftime('%s','now','start of month') AS INTEGER)",
            [],
            |row| row.get(0),
        )
        .map_err(ApiError::db)?;

    let skills = hub_counts(&db, "skills", month_start).map_err(ApiError::db)?;
    let agents = hub_counts(&db, "agents", month_start).map_err(ApiError::db)?;
    let downloads =
        skills["downloads"].as_i64().unwrap_or(0) + agents["downloads"].as_i64().unwrap_or(0);
    let new_this_month = skills["new_this_month"].as_i64().unwrap_or(0)
        + agents["new_this_month"].as_i64().unwrap_or(0);

    Ok(json!({
        "scope": "hub",
        "skills": skills,
        "agents": agents,
        "totals": {
            "skills": skills["total"],
            "agents": agents["total"],
            "downloads": downloads,
            "new_this_month": new_this_month,
        },
        "month_start": month_start,
        "generated_at": now(),
    }))
}

fn hub_counts(db: &Connection, table: &str, month_start: i64) -> rusqlite::Result<Value> {
    // `table` is a compile-time constant at both call sites, never input.
    let sql = format!(
        "SELECT COALESCE(SUM(CASE WHEN takedown_at IS NULL AND withdrawn_at IS NULL
                                  THEN 1 ELSE 0 END), 0) AS total,
                COALESCE(SUM(CASE WHEN takedown_at IS NULL AND withdrawn_at IS NULL
                                   AND visibility = 'public'
                                  THEN 1 ELSE 0 END), 0) AS public,
                COALESCE(SUM(CASE WHEN takedown_at IS NULL AND withdrawn_at IS NULL
                                   AND visibility = 'tenant'
                                  THEN 1 ELSE 0 END), 0) AS private,
                COALESCE(SUM(CASE WHEN takedown_at IS NULL AND withdrawn_at IS NULL
                                   AND created_at >= ?1
                                  THEN 1 ELSE 0 END), 0) AS new_this_month,
                -- Downloads are cumulative, matching the tenant dashboard.
                COALESCE(SUM(download_count), 0) AS downloads
           FROM {table}"
    );
    db.query_row(&sql, params![month_start], |row| {
        let total: i64 = row.get("total")?;
        let public: i64 = row.get("public")?;
        let private: i64 = row.get("private")?;
        let new_this_month: i64 = row.get("new_this_month")?;
        let downloads: i64 = row.get("downloads")?;
        Ok(json!({
            "total": total,
            "public": public,
            "private": private,
            "new_this_month": new_this_month,
            "downloads": downloads,
        }))
    })
}

fn counts(db: &Connection, table: &str, tenant: &str, month_start: i64) -> rusqlite::Result<Value> {
    // `table` is a compile-time constant at both call sites, never input.
    let sql = format!(
        "SELECT COALESCE(SUM(CASE WHEN takedown_at IS NULL AND withdrawn_at IS NULL
                                  THEN 1 ELSE 0 END), 0) AS total,
                COALESCE(SUM(CASE WHEN takedown_at IS NULL AND withdrawn_at IS NULL
                                   AND created_at >= ?2
                                  THEN 1 ELSE 0 END), 0) AS new_this_month,
                -- Downloads are cumulative: a later withdrawal/takedown must
                -- not erase history from the showcase number.
                COALESCE(SUM(download_count), 0) AS downloads
           FROM {table}
          WHERE visibility = 'public' OR owner_tenant = ?1"
    );
    db.query_row(&sql, params![tenant, month_start], |row| {
        let total: i64 = row.get("total")?;
        let new_this_month: i64 = row.get("new_this_month")?;
        let downloads: i64 = row.get("downloads")?;
        Ok(json!({
            "total": total,
            "new_this_month": new_this_month,
            "downloads": downloads,
        }))
    })
}
