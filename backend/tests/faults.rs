//! Fault injection: what the API does when the database misbehaves.
//!
//! Two properties matter for the refactor, because both are easy to lose while
//! merging `src/api.rs` and `src/agents.rs`:
//!
//! 1. a broken query is a **500 `db_error`**, never a 404/410 "not found" — the
//!    current code maps any `rusqlite` error to `not_found` in several places,
//!    which turns a database hiccup into "your package does not exist";
//! 2. a write and its audit entry are **one transaction** — a failed audit must
//!    not leave the state change behind.
//!
//! Faults are injected by renaming a table away through the harness connection:
//! every following statement fails with `no such table`, which is exactly the
//! shape of a real outage without any extra plumbing.

mod common;

use axum::http::StatusCode;
use common::{find, Harness};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug)]
enum Kind {
    Skill,
    Agent,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Skill, Kind::Agent];

    fn table(self) -> &'static str {
        match self {
            Kind::Skill => "skills",
            Kind::Agent => "agents",
        }
    }

    fn versions(self) -> &'static str {
        match self {
            Kind::Skill => "skill_versions",
            Kind::Agent => "agent_versions",
        }
    }

    fn list(self) -> &'static str {
        match self {
            Kind::Skill => "/api/v1/skills",
            Kind::Agent => "/api/v1/agents",
        }
    }

    fn action(self, slug: &str, action: &str) -> String {
        format!("{}/{slug}/{action}", self.list())
    }

    fn bundle(self, slug: &str, version: &str) -> String {
        format!("{}/{slug}/{version}/bundle", self.list())
    }

    async fn publish(
        self,
        h: &Harness,
        tenant: &str,
        slug: &str,
        version: &str,
    ) -> (StatusCode, Value) {
        match self {
            Kind::Skill => h.publish_skill(tenant, slug, version, "public").await,
            Kind::Agent => h.publish_agent(tenant, slug, version, "public").await,
        }
    }

    fn rename(self, h: &Harness, from: &str, to: &str) {
        h.exec(&format!("ALTER TABLE {from} RENAME TO {to}"));
        // Prove the fault is live before the request runs: a rename that
        // silently did nothing would make the whole test vacuous.
        assert!(
            h.db()
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [from],
                    |_| Ok(()),
                )
                .is_err(),
            "{from} still present after rename"
        );
    }

    fn restore(self, h: &Harness, from: &str, to: &str) {
        h.exec(&format!("ALTER TABLE {to} RENAME TO {from}"));
    }
}

/// A download whose query cannot run at all must not be reported as a missing
/// version or a vanished package.
#[tokio::test]
async fn a_download_failure_is_not_a_missing_package() {
    for kind in Kind::ALL {
        let h = Harness::new();
        let (status, body) = kind.publish(&h, "tenant-a", "alpha", "1.0.0").await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");

        let versions = kind.versions();
        kind.rename(&h, versions, "broken_versions");

        let (status, bytes) = h
            .download(&kind.bundle("alpha", "1.0.0"), Some("tenant-b"))
            .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{kind:?} download: {}",
            String::from_utf8_lossy(&bytes)
        );
        let body: Value = serde_json::from_slice(&bytes).expect("error json");
        assert_eq!(body["error"]["code"], "db_error", "{kind:?}: {body}");

        kind.restore(&h, versions, "broken_versions");
        let (status, _) = h
            .download(&kind.bundle("alpha", "1.0.0"), Some("tenant-b"))
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?} did not recover");
    }
}

/// A listing whose per-row subqueries cannot run must not degrade into an empty
/// market: "no packages" and "the database is down" are different answers.
#[tokio::test]
async fn a_listing_failure_is_not_an_empty_market() {
    for kind in Kind::ALL {
        let h = Harness::new();
        let (status, body) = kind.publish(&h, "tenant-a", "alpha", "1.0.0").await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");

        let versions = kind.versions();
        kind.rename(&h, versions, "broken_versions");
        let (status, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{kind:?}: {listing}");
        assert_eq!(listing["error"]["code"], "db_error", "{kind:?}: {listing}");

        kind.restore(&h, versions, "broken_versions");
        let (status, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {listing}");
        assert_eq!(listing["total"], 1, "{kind:?}");
    }
}

/// A write and its audit row are one unit of work: if the audit insert fails,
/// nothing else of that request survives.
#[tokio::test]
async fn a_publish_is_all_or_nothing() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.rename(&h, "audit_log", "broken_audit");

        let (status, body) = kind.publish(&h, "tenant-a", "alpha", "1.0.0").await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{kind:?}: {body}"
        );

        kind.restore(&h, "audit_log", "broken_audit");

        // The half-written publish must be gone: no entry in the market, and
        // nothing in `{table}` either (a leftover row would poison the slug).
        let (status, listing, _) = h
            .call("GET", kind.list(), Some("tenant-a"), None, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {listing}");
        assert_eq!(listing["total"], 0, "{kind:?} publish leaked a row: {listing}");
        let rows: i64 = h
            .db()
            .query_row(&format!("SELECT COUNT(*) FROM {}", kind.table()), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "{kind:?} left a row in {}", kind.table());

        // The slug is still free, so the retry succeeds.
        let (status, body) = kind.publish(&h, "tenant-a", "alpha", "1.0.0").await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?} retry: {body}");
        let (_, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        assert_eq!(find(&listing, "alpha")["latest_version"], "1.0.0", "{kind:?}");
    }
}

/// Same property on the state-change paths. Ignored until the transaction is
/// actually there: `withdraw`, `restore` and `takedown` run the `UPDATE` and
/// the audit `INSERT` as two separate statements, so today the flag sticks
/// while the request fails.
#[tokio::test]
async fn a_state_change_is_all_or_nothing() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "1.0.0").await;

        kind.rename(&h, "audit_log", "broken_audit");
        let (status, body, _) = h
            .call(
                "POST",
                &kind.action("alpha", "withdraw"),
                Some("tenant-a"),
                Some(json!({ "reason": "retired" })),
                None,
            )
            .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{kind:?}: {body}"
        );
        kind.restore(&h, "audit_log", "broken_audit");

        let (_, status_body) = {
            let (status, body, _) = h
                .call(
                    "GET",
                    &kind.action("alpha", "status"),
                    Some("tenant-a"),
                    None,
                    None,
                )
                .await;
            (status, body)
        };
        assert_eq!(
            status_body["state"], "active",
            "{kind:?} withdraw stuck despite the failed audit"
        );

        // And the same for an admin takedown.
        let cookie = h.login().await;
        kind.rename(&h, "audit_log", "broken_audit");
        let (status, body, _) = h
            .call(
                "POST",
                &format!("/admin/api/{}/{}/takedown", kind.table(), "alpha"),
                None,
                Some(json!({ "reason": "policy" })),
                Some(&cookie),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{kind:?}: {body}"
        );
        kind.restore(&h, "audit_log", "broken_audit");
        let (_, status_body) = {
            let (status, body, _) = h
                .call(
                    "GET",
                    &kind.action("alpha", "status"),
                    Some("tenant-a"),
                    None,
                    None,
                )
                .await;
            (status, body)
        };
        assert_eq!(
            status_body["state"], "active",
            "{kind:?} takedown stuck despite the failed audit"
        );
    }
}

/// An admin listing that cannot read its join must fail loudly as well.
#[tokio::test]
async fn admin_listing_failure_is_a_server_error() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "1.0.0").await;
        let cookie = h.login().await;

        kind.rename(&h, "tenants", "broken_tenants");
        let (status, body, _) = h
            .call(
                "GET",
                &format!("/admin/api/{}", kind.table()),
                None,
                None,
                Some(&cookie),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{kind:?}: {body}"
        );
        assert_eq!(body["error"]["code"], "db_error", "{kind:?}: {body}");
        kind.restore(&h, "tenants", "broken_tenants");

        let (status, body, _) = h
            .call(
                "GET",
                &format!("/admin/api/{}", kind.table()),
                None,
                None,
                Some(&cookie),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        assert_eq!(body["items"].as_array().unwrap().len(), 1, "{kind:?}");
    }
}
