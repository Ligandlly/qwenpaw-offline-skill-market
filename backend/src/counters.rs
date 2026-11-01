//! Process-local counters, written back in batches.
//!
//! Two numbers here are telemetry rather than state: a tenant's `last_seen`
//! beacon and an entry's `download_count`. Recording them straight from the read
//! path made every listing GET a write transaction and every bundle download a
//! second one — and with one SQLite connection behind a mutex, the read path is
//! the worst possible place to fight for the write lock.
//!
//! So they are accumulated in memory and flushed by one background task: a GET
//! costs no I/O at all, and the database sees one small transaction per
//! interval. The price is that both numbers can lag by up to one interval and a
//! crash loses the last few seconds of counting. For a popularity figure that is
//! a fair trade — and much cheaper than the Redis this product cannot assume,
//! since the market ships as one offline binary.

use rusqlite::params;
use std::collections::HashMap;

use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::state::Shared;

/// How often the accumulated numbers reach SQLite.
pub const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// What is waiting to be written back.
#[derive(Default)]
pub struct Pending {
    /// `(table, slug)` -> fetches to add to `download_count`.
    downloads: HashMap<(&'static str, String), i64>,
    /// tenant -> newest moment it was seen.
    seen: HashMap<String, i64>,
}

impl Pending {
    /// Counts one successful bundle fetch. Memory only: see the module docs.
    pub fn add_download(&mut self, table: &'static str, slug: &str) {
        *self
            .downloads
            .entry((table, slug.to_string()))
            .or_insert(0) += 1;
    }

    /// The market listing is the only traffic the backend observes, so it
    /// doubles as the "tenant is alive" beacon.
    pub fn touch(&mut self, tenant: &str) {
        let at = now();
        let seen = self.seen.entry(tenant.to_string()).or_insert(at);
        if at > *seen {
            *seen = at;
        }
    }

    fn take(&mut self) -> Snapshot {
        Snapshot {
            downloads: self.downloads.drain().collect(),
            seen: self.seen.drain().collect(),
        }
    }

    /// Puts a failed batch back so the next interval retries it.
    fn restore(&mut self, snapshot: Snapshot) {
        for (key, count) in snapshot.downloads {
            *self.downloads.entry(key).or_insert(0) += count;
        }
        for (tenant, at) in snapshot.seen {
            let seen = self.seen.entry(tenant).or_insert(at);
            if at > *seen {
                *seen = at;
            }
        }
    }
}

struct Snapshot {
    downloads: Vec<((&'static str, String), i64)>,
    seen: Vec<(String, i64)>,
}

impl Snapshot {
    fn is_empty(&self) -> bool {
        self.downloads.is_empty() && self.seen.is_empty()
    }

    fn rows(&self) -> usize {
        self.downloads.len() + self.seen.len()
    }
}

/// Writes everything accumulated so far, in one transaction.
///
/// Callers are the background task and the harness; a failure puts the batch
/// back in memory, so the numbers are retried instead of dropped.
pub fn flush(app: &Shared) -> ApiResult<usize> {
    // Take the snapshot before touching the database: the two locks must never
    // be held at the same time, or a flusher and a request can deadlock.
    let snapshot = app.pending().take();
    if snapshot.is_empty() {
        return Ok(0);
    }
    match write_back(app, &snapshot) {
        Ok(()) => Ok(snapshot.rows()),
        Err(err) => {
            app.pending().restore(snapshot);
            Err(err)
        }
    }
}

fn write_back(app: &Shared, snapshot: &Snapshot) -> ApiResult<()> {
    let db = app.db();
    let tx = db.unchecked_transaction().map_err(ApiError::db)?;
    for ((table, slug), count) in &snapshot.downloads {
        tx.execute(
            &format!("UPDATE {table} SET download_count = download_count + ?2 WHERE slug = ?1"),
            params![slug, count],
        )
        .map_err(ApiError::db)?;
    }
    for (tenant, at) in &snapshot.seen {
        tx.execute(
            "INSERT INTO tenants (id, first_seen, last_seen) VALUES (?1, ?2, ?2)
             ON CONFLICT(id) DO UPDATE SET last_seen = MAX(tenants.last_seen, excluded.last_seen)",
            params![tenant, at],
        )
        .map_err(ApiError::db)?;
    }
    tx.commit().map_err(ApiError::db)
}

/// Flushes every [`FLUSH_INTERVAL`] until `stop` flips, then once more so the
/// last seconds before shutdown are not lost.
pub async fn flush_loop(app: Shared, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => flush_quietly(&app, "periodic"),
            _ = stop.changed() => {
                flush_quietly(&app, "shutdown");
                return;
            }
        }
    }
}

fn flush_quietly(app: &Shared, why: &str) {
    match flush(app) {
        Ok(0) => {}
        Ok(rows) => tracing::debug!(rows, why, "counters flushed"),
        Err(err) => tracing::warn!(
            code = err.code,
            why,
            "counter flush failed; the batch stays in memory for the next attempt"
        ),
    }
}

