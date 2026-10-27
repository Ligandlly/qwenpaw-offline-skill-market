//! Dashboard aggregates for the tenant-facing overview tab.
//!
//! Counts mirror what the market listing shows this tenant: active public
//! items plus its own active private ones. Takedown/withdrawn rows are
//! excluded on purpose — the dashboard is a showcase, not an archive.
//! `download_count` is incremented by the bundle handlers after a successful
//! fetch, so it means "successful bundle downloads", not listing views.

use axum::extract::State;
use axum::Json;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::api::{ApiError, ApiResult};
use crate::auth::TenantId;
use crate::db::now;
use crate::state::Shared;

pub async fn overview(State(app): State<Shared>, tenant: TenantId) -> ApiResult<Json<Value>> {
    let db = app.db();
    let month_start: i64 = db
        .query_row(
            "SELECT CAST(strftime('%s','now','start of month') AS INTEGER)",
            [],
            |row| row.get(0),
        )
        .map_err(ApiError::db)?;

    let skills = counts(&db, "skills", &tenant.0, month_start).map_err(ApiError::db)?;
    let agents = counts(&db, "agents", &tenant.0, month_start).map_err(ApiError::db)?;
    let downloads =
        skills["downloads"].as_i64().unwrap_or(0) + agents["downloads"].as_i64().unwrap_or(0);
    let new_this_month = skills["new_this_month"].as_i64().unwrap_or(0)
        + agents["new_this_month"].as_i64().unwrap_or(0);

    Ok(Json(json!({
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
    })))
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
