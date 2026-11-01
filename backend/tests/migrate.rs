//! Schema evolution: a market that is already running must be brought forward
//! once, and a database from the future must be refused rather than
//! half-understood.

use rusqlite::Connection;

/// The schema a v0.1.7 database has: everything except the listing summary
/// columns (`manifest_text` / `version_count` / `size_bytes` / `updated_at` /
/// skill_count`) and without any `user_version` stamp.
const OLD_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tenants (
    id           TEXT PRIMARY KEY,
    first_seen   INTEGER NOT NULL,
    last_seen    INTEGER NOT NULL,
    display_name TEXT
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
    download_count   INTEGER NOT NULL DEFAULT 0
);
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
"#;

fn temp_db(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("market-migrate-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("market.sqlite3")
}

fn user_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("read user_version")
}

/// A database as the previous release left it, with one published version each.
fn old_market(path: &std::path::Path) {
    let conn = Connection::open(path).expect("create the old database");
    conn.execute_batch(OLD_SCHEMA).expect("old schema");
    conn.execute_batch(
        r#"
INSERT INTO tenants (id, first_seen, last_seen) VALUES ('tenant-a', 100, 100);
INSERT INTO skills (slug, name, owner_tenant, visibility, latest_version, created_at)
     VALUES ('demo', 'Demo', 'tenant-a', 'public', '1.0.0', 100);
INSERT INTO skill_versions (slug, version, sha256, size_bytes, file_count, bundle_json, created_at)
     VALUES ('demo', '1.0.0', 'digest', 60, 1,
             '{"SKILL.md":"---\nname: Demo\ndescription: backfilled\n---\n"}', 150);
INSERT INTO agents (slug, name, description, owner_tenant, visibility, latest_version, created_at)
     VALUES ('qa', 'QA', 'answers', 'tenant-a', 'public', '1.0.0', 100);
INSERT INTO agent_versions (slug, version, sha256, size_bytes, file_count, skill_count, bundle_json, created_at)
     VALUES ('qa', '1.0.0', 'digest', 40, 2, 3, '{}', 160);
"#,
    )
    .expect("old rows");
}

#[test]
fn a_fresh_database_is_stamped_with_the_current_version() {
    let path = temp_db("fresh");
    let conn = offline_market_backend::db::open(&path).expect("open a fresh database");
    assert_eq!(
        user_version(&conn),
        offline_market_backend::db::SCHEMA_VERSION
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn an_old_database_gets_the_new_columns_and_no_loss() {
    let path = temp_db("upgrade");
    old_market(&path);

    let conn = offline_market_backend::db::open(&path).expect("upgrade the old database");
    assert_eq!(
        user_version(&conn),
        offline_market_backend::db::SCHEMA_VERSION
    );

    // The published row survives, and the listing columns are derived from the
    // rows that were already there.
    let (name, manifest, versions, size, updated): (String, Option<String>, i64, i64, i64) = conn
        .query_row(
            "SELECT name, manifest_text, version_count, size_bytes, updated_at
               FROM skills WHERE slug = 'demo'",
            [],
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
        .expect("the skill row");
    assert_eq!(name, "Demo");
    assert!(
        manifest.is_some_and(|text| text.contains("backfilled")),
        "the latest SKILL.md must be copied onto the row"
    );
    assert_eq!((versions, size, updated), (1, 60, 150));

    let (skills, sizes, updated): (i64, i64, i64) = conn
        .query_row(
            "SELECT skill_count, size_bytes, updated_at FROM agents WHERE slug = 'qa'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the agent row");
    assert_eq!((skills, sizes, updated), (3, 40, 160));

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_upgrade_runs_once_per_database() {
    let path = temp_db("once");
    old_market(&path);
    {
        let conn = offline_market_backend::db::open(&path).expect("first upgrade");
        conn.execute("UPDATE skills SET version_count = 42", [])
            .expect("tamper with a derived column");
    }

    // Re-opening must not re-run the backfill: it would silently overwrite
    // whatever the running market has since computed.
    let conn = offline_market_backend::db::open(&path).expect("second open");
    let versions: i64 = conn
        .query_row(
            "SELECT version_count FROM skills WHERE slug = 'demo'",
            [],
            |row| row.get(0),
        )
        .expect("the skill row");
    assert_eq!(versions, 42);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_newer_database_is_refused_instead_of_half_read() {
    let path = temp_db("newer");
    {
        let conn = offline_market_backend::db::open(&path).expect("create the database");
        conn.pragma_update(None, "user_version", 99i64)
            .expect("stamp a future version");
    }

    let err = offline_market_backend::db::open(&path).expect_err("a future schema must not load");
    let message = err.to_string();
    assert!(message.contains("99"), "{message}");
    assert!(
        message.contains(&offline_market_backend::db::SCHEMA_VERSION.to_string()),
        "{message}"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
