//! SQLite schema + thin helpers. No ORM: the schema is small and read-mostly.

use rusqlite::Connection;
use std::path::Path;

pub const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS tenants (
    id          TEXT PRIMARY KEY,
    first_seen  INTEGER NOT NULL,
    last_seen   INTEGER NOT NULL
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
    takedown_reason  TEXT
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

CREATE TABLE IF NOT EXISTS audit_log (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    at      INTEGER NOT NULL,
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
    Ok(conn)
}

pub fn open_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
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
    conn.execute(
        "INSERT INTO audit_log (at, actor, action, slug, version, detail) VALUES (?1,?2,?3,?4,?5,?6)",
        rusqlite::params![now(), actor, action, slug, version, detail],
    )?;
    Ok(())
}
