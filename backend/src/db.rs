//! SQLite schema + thin helpers. No ORM: the schema is small and read-mostly.

use rusqlite::Connection;
use std::path::Path;

pub const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS tenants (
    id           TEXT PRIMARY KEY,
    first_seen   INTEGER NOT NULL,
    last_seen    INTEGER NOT NULL,
    -- Publisher name the plugin reports (the tenant's own login name). Purely
    -- cosmetic: ownership is *always* `owner_tenant`, never this column.
    display_name TEXT
);

CREATE TABLE IF NOT EXISTS admin_users (
    username      TEXT PRIMARY KEY,
    password_hash TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS skills (
    slug             TEXT PRIMARY KEY,
    name             TEXT NOT NULL,
    owner_tenant     TEXT NOT NULL,
    visibility       TEXT NOT NULL CHECK (visibility IN ('public','tenant')),
    latest_version   TEXT,
    created_at       INTEGER NOT NULL,
    withdrawn_at     INTEGER,
    withdrawn_by     TEXT,
    takedown_at      INTEGER,
    takedown_by      TEXT,
    takedown_reason  TEXT,
    -- Successful bundle fetches (installs). A counter, not an audit row:
    -- a dashboard needs cheap SUM(), and old rows start at zero.
    download_count   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_skills_owner ON skills(owner_tenant);

CREATE TABLE IF NOT EXISTS skill_versions (
    slug        TEXT NOT NULL REFERENCES skills(slug) ON DELETE CASCADE,
    version     TEXT NOT NULL,
    sha256      TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL,
    file_count  INTEGER NOT NULL,
    bundle_json TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (slug, version)
);

CREATE TABLE IF NOT EXISTS agents (
    slug             TEXT PRIMARY KEY,
    name             TEXT NOT NULL,
    description      TEXT NOT NULL DEFAULT '',
    owner_tenant     TEXT NOT NULL,
    visibility       TEXT NOT NULL CHECK (visibility IN ('public','tenant')),
    latest_version   TEXT,
    created_at       INTEGER NOT NULL,
    withdrawn_at     INTEGER,
    withdrawn_by     TEXT,
    takedown_at      INTEGER,
    takedown_by      TEXT,
    takedown_reason  TEXT,
    download_count   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_agents_owner ON agents(owner_tenant);

CREATE TABLE IF NOT EXISTS agent_versions (
    slug        TEXT NOT NULL REFERENCES agents(slug) ON DELETE CASCADE,
    version     TEXT NOT NULL,
    sha256      TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL,
    file_count  INTEGER NOT NULL,
    skill_count INTEGER NOT NULL,
    bundle_json TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (slug, version)
);

CREATE TABLE IF NOT EXISTS audit_log (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    at      INTEGER NOT NULL,
    kind    TEXT NOT NULL DEFAULT 'skill',
    actor   TEXT NOT NULL,
    action  TEXT NOT NULL,
    slug    TEXT,
    version TEXT,
    detail  TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_at ON audit_log(at);
"#;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// `CREATE TABLE IF NOT EXISTS` cannot add columns to a market that is
/// already running, so older databases get the new ones here. Idempotent.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    ensure_column(conn, "tenants", "display_name", "TEXT")?;
    ensure_column(conn, "audit_log", "kind", "TEXT NOT NULL DEFAULT 'skill'")?;
    ensure_column(
        conn,
        "skills",
        "download_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "agents",
        "download_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> rusqlite::Result<()> {
    let columns: Vec<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    if !columns.iter().any(|name| name == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
        tracing::info!(table, column, "schema: column added");
    }
    Ok(())
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn audit(
    conn: &Connection,
    actor: &str,
    action: &str,
    slug: Option<&str>,
    version: Option<&str>,
    detail: Option<&str>,
) -> rusqlite::Result<()> {
    audit_kind(conn, "skill", actor, action, slug, version, detail)
}

/// Same audit trail, tagged so the admin UI can tell skills from agents.
pub fn audit_kind(
    conn: &Connection,
    kind: &str,
    actor: &str,
    action: &str,
    slug: Option<&str>,
    version: Option<&str>,
    detail: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO audit_log (at, kind, actor, action, slug, version, detail)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![now(), kind, actor, action, slug, version, detail],
    )?;
    let slug = slug.unwrap_or("-");
    let version = version.unwrap_or("-");
    tracing::debug!(
        kind = %kind,
        actor = %actor,
        action = %action,
        slug = %slug,
        version = %version,
        "audit"
    );
    Ok(())
}
