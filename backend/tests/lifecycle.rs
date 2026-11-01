//! Lifecycle contract: what a published version means, who may change its
//! state, and what the audit trail records. Same assertions on both planes —
//! this is the second half of the safety net for merging `src/api.rs` and
//! `src/agents.rs` into one implementation.

mod common;

use axum::http::StatusCode;
use common::{agent_files, find, skill_files, slugs, Harness, ADMIN_PASSWORD, ADMIN_USER};
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

    fn audit_kind(self) -> &'static str {
        match self {
            Kind::Skill => "skill",
            Kind::Agent => "agent",
        }
    }

    fn list(self) -> &'static str {
        match self {
            Kind::Skill => "/api/v1/skills",
            Kind::Agent => "/api/v1/agents",
        }
    }

    fn mine(self) -> String {
        format!("{}/mine", self.list())
    }

    fn action(self, slug: &str, action: &str) -> String {
        format!("{}/{slug}/{action}", self.list())
    }

    fn bundle(self, slug: &str, version: &str) -> String {
        format!("{}/{slug}/{version}/bundle", self.list())
    }

    fn admin_action(self, slug: &str, action: &str) -> String {
        format!("/admin/api/{}/{slug}/{action}", self.table())
    }

    fn files(self, marker: &str) -> Value {
        match self {
            Kind::Skill => {
                let mut files = skill_files();
                files["SKILL.md"] = json!(format!(
                    "---\nname: demo-skill\ndescription: {marker}\n---\n\n# {marker}\n"
                ));
                files
            }
            Kind::Agent => agent_files("QA Agent", marker),
        }
    }

    async fn publish(
        self,
        h: &Harness,
        tenant: &str,
        slug: &str,
        version: &str,
        visibility: &str,
    ) -> (StatusCode, Value) {
        match self {
            Kind::Skill => h.publish_skill(tenant, slug, version, visibility).await,
            Kind::Agent => h.publish_agent(tenant, slug, version, visibility).await,
        }
    }

    async fn publish_with(
        self,
        h: &Harness,
        tenant: &str,
        slug: &str,
        version: &str,
        visibility: &str,
        marker: &str,
    ) -> (StatusCode, Value) {
        let files = self.files(marker);
        match self {
            Kind::Skill => {
                h.publish_skill_with(
                    tenant,
                    json!({
                        "name": format!("Skill {slug}"),
                        "slug": slug,
                        "version": version,
                        "visibility": visibility,
                        "files": files
                    }),
                )
                .await
            }
            Kind::Agent => {
                h.publish_agent_with(
                    tenant,
                    json!({
                        "slug": slug,
                        "version": version,
                        "visibility": visibility,
                        "publisher_name": "张三",
                        "files": files
                    }),
                )
                .await
            }
        }
    }

    async fn admin(&self, h: &Harness, slug: &str, action: &str) -> (StatusCode, Value) {
        let cookie = h.login().await;
        let (status, body, _) = h
            .call(
                "POST",
                &format!("/admin/api/{}/{slug}/{action}", self.table()),
                None,
                Some(json!({ "reason": "policy violation" })),
                Some(&cookie),
            )
            .await;
        (status, body)
    }

    async fn status_of(self, h: &Harness, tenant: &str, slug: &str) -> (StatusCode, Value) {
        let (status, body, _) = h
            .call("GET", &self.action(slug, "status"), Some(tenant), None, None)
            .await;
        (status, body)
    }
}

/// A bundle is a snapshot of one version: publishing another version must not
/// change a single byte of it, on either plane.
fn assert_same_bundle(before: &[u8], after: &[u8]) {
    assert_eq!(
        String::from_utf8_lossy(after),
        String::from_utf8_lossy(before),
        "bundle payload changed"
    );
}

// ---------------------------------------------------------------------------
// versions are append-only
// ---------------------------------------------------------------------------

/// A publish body that keeps the slug but relabels the package: `name` lives in
/// the manifest, so a skill renames through SKILL.md and an agent through
/// `manifest.json`.
fn relabel(kind: Kind, name: &str, version: &str, visibility: &str) -> Value {
    let files = match kind {
        Kind::Skill => json!({
            "SKILL.md": format!("---\nname: {name}\ndescription: {name} 的说明\n---\n\n# {name}\n")
        }),
        Kind::Agent => agent_files(name, &format!("{name} 的说明")),
    };
    json!({
        "name": name,
        "slug": "alpha",
        "version": version,
        "visibility": visibility,
        "files": files,
    })
}

async fn publish_as(kind: Kind, h: &Harness, body: Value) {
    let (status, answer) = match kind {
        Kind::Skill => h.publish_skill_with("tenant-a", body).await,
        Kind::Agent => h.publish_agent_with("tenant-a", body).await,
    };
    assert_eq!(status, StatusCode::CREATED, "{kind:?}: {answer}");
}

#[tokio::test]
async fn the_listing_follows_the_newest_version_not_the_last_publish() {
    for kind in Kind::ALL {
        let h = Harness::new();
        publish_as(kind, &h, relabel(kind, "Newest", "2.0.0", "public")).await;

        // An older version arrives late with another name: the listing must not
        // follow it backwards, but the request's visibility does apply.
        publish_as(kind, &h, relabel(kind, "Older", "1.5.0", "tenant")).await;
        let (_, listing, _) = h.call("GET", kind.list(), Some("tenant-a"), None, None).await;
        let item = find(&listing, "alpha");
        assert_eq!(item["name"], "Newest", "{kind:?} listing followed an old publish");
        assert_eq!(item["latest_version"], "2.0.0", "{kind:?}");
        assert_eq!(item["visibility"], "tenant", "{kind:?}");

        // Which means another team no longer sees it at all.
        let (_, other, _) = h.call("GET", kind.list(), Some("tenant-b"), None, None).await;
        assert_eq!(slugs(&other), Vec::<String>::new(), "{kind:?} leaked a tenant entry");

        // Forward again: the listing takes the new name and the new visibility.
        publish_as(kind, &h, relabel(kind, "Newer", "2.1.0", "public")).await;
        let (_, listing, _) = h.call("GET", kind.list(), Some("tenant-b"), None, None).await;
        let item = find(&listing, "alpha");
        assert_eq!(item["name"], "Newer", "{kind:?}");
        assert_eq!(item["latest_version"], "2.1.0", "{kind:?}");
        assert_eq!(item["visibility"], "public", "{kind:?}");
    }
}

#[tokio::test]
async fn a_published_version_is_immutable() {
    for kind in Kind::ALL {
        let h = Harness::new();
        let (status, body) = kind
            .publish_with(&h, "tenant-a", "alpha", "1.0.0", "public", "first")
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");
        let (status, first, ) = h.download(&kind.bundle("alpha", "1.0.0"), Some("tenant-b")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");

        // A second version is accepted and becomes latest.
        let (status, body) = kind
            .publish_with(&h, "tenant-a", "alpha", "1.1.0", "public", "second")
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");
        assert_eq!(body["latest_version"], "1.1.0", "{kind:?}");

        // ... and the old one still serves exactly the bytes it was published
        // with: a version is a fact, not a mutable row.
        let (status, again) = h.download(&kind.bundle("alpha", "1.0.0"), Some("tenant-b")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        assert_same_bundle(&first, &again);
        let bundle: Value = serde_json::from_slice(&first).unwrap();
        assert!(
            bundle["files"].get("SKILL.md").is_none_or(|v| v
                .as_str()
                .is_some_and(|s| !s.contains("second"))),
            "{kind:?} 1.0.0 was rewritten: {bundle}"
        );

        // Each version describes itself: 1.1.0's manifest marker is "second",
        // 1.0.0's stays "first" — the payload is not read from the listing row.
        let (status, latest) = h.download(&kind.bundle("alpha", "1.1.0"), Some("tenant-b")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        let latest: Value = serde_json::from_slice(&latest).unwrap();
        let described = match kind {
            Kind::Skill => latest["files"]["SKILL.md"].as_str().unwrap().contains("second"),
            Kind::Agent => latest["description"] == "second",
        };
        assert!(described, "{kind:?} 1.1.0 does not carry its own manifest: {latest}");

        // Re-publishing a taken version is rejected, and rejection must not
        // mutate anything: the API is a rollback, not a partial write.
        let (status, body) = kind
            .publish_with(&h, "tenant-a", "alpha", "1.0.0", "tenant", "third")
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{kind:?}: {body}");
        assert_eq!(body["error"]["code"], "version_exists", "{kind:?}");

        let (status, listing, _) = h
            .call("GET", &kind.mine(), Some("tenant-a"), None, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        let item = find(&listing, "alpha");
        assert_eq!(item["versions"], 2, "{kind:?} rejected publish added a version");
        assert_eq!(item["latest_version"], "1.1.0", "{kind:?}");
        assert_eq!(
            item["visibility"], "public",
            "{kind:?} rejected publish changed visibility"
        );
        assert!(
            item["name"].as_str().is_some_and(|n| !n.contains("1.0.0")),
            "{kind:?} rejected publish rewrote the name: {}",
            item["name"]
        );

        let (status, kept) = h.download(&kind.bundle("alpha", "1.0.0"), Some("tenant-b")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        assert_same_bundle(&first, &kept);
    }
}

#[tokio::test]
async fn latest_never_rolls_back_and_a_publish_revives_a_withdrawal() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "2.0.0", "public").await;

        // A late publish of an older version is stored but must not move
        // `latest` backwards — the market never "downgrades" by accident.
        let (status, body) = kind
            .publish(&h, "tenant-a", "alpha", "1.0.0", "public")
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");
        assert_eq!(body["latest_version"], "2.0.0", "{kind:?} rolled back");

        let (_, status_body) = kind.status_of(&h, "tenant-b", "alpha").await;
        assert_eq!(status_body["latest_version"], "2.0.0", "{kind:?}");
        assert_eq!(status_body["state"], "active", "{kind:?}");

        // Withdrawing, then publishing a new version, revives the entry: a
        // publish is an explicit "we are shipping again".
        let (status, body, _) = h
            .call(
                "POST",
                &kind.action("alpha", "withdraw"),
                Some("tenant-a"),
                Some(json!({ "reason": "changing hands" })),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        let (_, status_body) = kind.status_of(&h, "tenant-b", "alpha").await;
        assert_eq!(status_body["state"], "withdrawn", "{kind:?}");

        let (status, body) = kind
            .publish(&h, "tenant-a", "alpha", "2.1.0", "public")
            .await;
        assert_eq!(status, StatusCode::CREATED, "{kind:?}: {body}");
        let (status, mine, _) = h
            .call("GET", &kind.mine(), Some("tenant-a"), None, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {mine}");
        assert!(
            find(&mine, "alpha")["withdrawn_at"].is_null(),
            "{kind:?} publish did not clear the withdrawal"
        );
        let (_, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        assert_eq!(slugs(&listing), ["alpha"], "{kind:?} not back in market");
    }
}

#[tokio::test]
async fn a_slug_belongs_to_its_first_team() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;

        // Another team cannot publish into an existing slug, at any version.
        let (status, body) = kind.publish(&h, "tenant-b", "alpha", "1.0.0", "public").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind:?}: {body}");
        assert_eq!(body["error"]["code"], "forbidden", "{kind:?}: {body}");

        let (status, body) = kind.publish(&h, "tenant-b", "alpha", "2.0.0", "public").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind:?}: {body}");
        assert_eq!(body["error"]["code"], "forbidden", "{kind:?}: {body}");

        // A takedown does not release the slug either.
        let (status, body) = kind.admin(&h, "alpha", "takedown").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        let (status, body) = kind.publish(&h, "tenant-b", "alpha", "1.0.0", "public").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind:?}: {body}");
        assert_eq!(body["error"]["code"], "forbidden", "{kind:?}: {body}");
    }
}

// ---------------------------------------------------------------------------
// withdraw / restore
// ---------------------------------------------------------------------------

#[tokio::test]
async fn withdraw_and_restore_are_idempotent_and_owner_only() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;

        // Not the owner: rejected, and nothing changes.
        let (status, body, _) = h
            .call(
                "POST",
                &kind.action("alpha", "withdraw"),
                Some("tenant-b"),
                Some(json!({})),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind:?}: {body}");
        assert_eq!(body["error"]["code"], "forbidden", "{kind:?}");
        let (_, status_body) = kind.status_of(&h, "tenant-b", "alpha").await;
        assert_eq!(status_body["state"], "active", "{kind:?}");

        // Owner: withdrawn, and doing it twice is not an error.
        for _ in 0..2 {
            let (status, body, _) = h
                .call(
                    "POST",
                    &kind.action("alpha", "withdraw"),
                    Some("tenant-a"),
                    Some(json!({ "reason": "retired" })),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
            assert_eq!(body["state"], "withdrawn", "{kind:?}");
        }
        let (_, status_body) = kind.status_of(&h, "tenant-b", "alpha").await;
        assert_eq!(status_body["state"], "withdrawn", "{kind:?}");
        let (_, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        assert_eq!(listing["total"], 0, "{kind:?} withdrawn item still listed");
        // The owner keeps access to its own withdrawn entry.
        let (status, _) = h.download(&kind.bundle("alpha", "1.0.0"), Some("tenant-a")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");

        // Restore, twice.
        for _ in 0..2 {
            let (status, body, _) = h
                .call(
                    "POST",
                    &kind.action("alpha", "restore"),
                    Some("tenant-a"),
                    Some(json!({})),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
            assert_eq!(body["state"], "active", "{kind:?}");
        }
        let (_, status_body) = kind.status_of(&h, "tenant-b", "alpha").await;
        assert_eq!(status_body["state"], "active", "{kind:?}");
        let (_, listing, _) = h
            .call("GET", kind.list(), Some("tenant-b"), None, None)
            .await;
        assert_eq!(slugs(&listing), ["alpha"], "{kind:?} not restored");

        // Unknown slug: 404, not a leak of anything else.
        let (status, body, _) = h
            .call(
                "POST",
                &kind.action("nope", "withdraw"),
                Some("tenant-a"),
                Some(json!({})),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{kind:?}: {body}");
        assert_eq!(body["error"]["code"], "not_found", "{kind:?}");
    }
}

// ---------------------------------------------------------------------------
// admin takedown / restore
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_takedown_outranks_the_owner() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;

        let (status, body) = kind.admin(&h, "alpha", "takedown").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        assert_eq!(body["state"], "taken_down", "{kind:?}");

        // The reason is visible to the owner through the status probe.
        let (_, status_body) = kind.status_of(&h, "tenant-a", "alpha").await;
        assert_eq!(status_body["state"], "taken_down", "{kind:?}");
        assert_eq!(status_body["takedown_reason"], "policy violation", "{kind:?}");

        // Nobody can serve, revive or re-publish a taken-down entry — not even
        // its owner, not even with a brand new version.
        let (status, _) = h.download(&kind.bundle("alpha", "1.0.0"), Some("tenant-a")).await;
        assert_eq!(status, StatusCode::GONE, "{kind:?} served a taken-down bundle");
        for action in ["withdraw", "restore"] {
            let (status, body, _) = h
                .call(
                    "POST",
                    &kind.action("alpha", action),
                    Some("tenant-a"),
                    Some(json!({})),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::CONFLICT, "{kind:?} {action}: {body}");
            assert_eq!(body["error"]["code"], "taken_down", "{kind:?} {action}");
        }
        let (status, body) = kind.publish(&h, "tenant-a", "alpha", "2.0.0", "public").await;
        assert_eq!(status, StatusCode::CONFLICT, "{kind:?}: {body}");
        assert_eq!(body["error"]["code"], "taken_down", "{kind:?}");

        // Only an admin lifts it.
        let (status, body) = kind.admin(&h, "alpha", "restore").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        assert_eq!(body["state"], "active", "{kind:?}");
        let (status, _) = h.download(&kind.bundle("alpha", "1.0.0"), Some("tenant-b")).await;
        assert_eq!(status, StatusCode::OK, "{kind:?} not serving again");

        // The owner's own restore still works afterwards (nothing left over).
        let (status, body, _) = h
            .call(
                "POST",
                &kind.action("alpha", "restore"),
                Some("tenant-a"),
                Some(json!({})),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");

        // Admin actions on an unknown slug are 404, not silent successes.
        let (status, body) = kind.admin(&h, "nope", "takedown").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{kind:?}: {body}");
    }
}

#[tokio::test]
async fn admin_listing_sees_every_state() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "active", "1.0.0", "public").await;
        kind.publish(&h, "tenant-a", "hidden", "1.0.0", "tenant").await;
        kind.publish(&h, "tenant-a", "retired", "1.0.0", "public").await;
        kind.publish(&h, "tenant-a", "banned", "1.0.0", "public").await;
        h.call(
            "POST",
            &kind.action("retired", "withdraw"),
            Some("tenant-a"),
            Some(json!({})),
            None,
        )
        .await;
        kind.admin(&h, "banned", "takedown").await;

        let cookie = h.login().await;
        let (status, body, _) = h
            .call("GET", &format!("/admin/api/{}", kind.table()), None, None, Some(&cookie))
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        assert_eq!(
            body["items"].as_array().unwrap().len(),
            4,
            "{kind:?} admin listing hides rows: {body}"
        );
        let banned = find(&body, "banned");
        assert!(banned["takedown_at"].as_i64().unwrap() > 0, "{kind:?}");
        assert_eq!(banned["takedown_reason"], "policy violation", "{kind:?}");
        assert!(
            find(&body, "retired")["withdrawn_at"].as_i64().unwrap() > 0,
            "{kind:?}"
        );

        // `q` filters on slug or display name.
        let (_, filtered, _) = h
            .call(
                "GET",
                &format!("/admin/api/{}?q=reti", kind.table()),
                None,
                None,
                Some(&cookie),
            )
            .await;
        assert_eq!(slugs(&filtered), ["retired"], "{kind:?} q filter: {filtered}");

        // Without an admin cookie: no listing at all.
        let (status, _, _) = h
            .call("GET", &format!("/admin/api/{}", kind.table()), None, None, None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{kind:?}");
    }
}

// ---------------------------------------------------------------------------
// audit trail
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_transition_is_audited() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;
        h.call(
            "POST",
            &kind.action("alpha", "withdraw"),
            Some("tenant-a"),
            Some(json!({ "reason": "retired" })),
            None,
        )
        .await;
        h.call(
            "POST",
            &kind.action("alpha", "restore"),
            Some("tenant-a"),
            Some(json!({})),
            None,
        )
        .await;
        kind.admin(&h, "alpha", "takedown").await;
        kind.admin(&h, "alpha", "restore").await;

        let cookie = h.login().await;
        let uri = format!("/admin/api/audit?kind={}", kind.audit_kind());
        let (status, body, _) = h
            .call("GET", &uri, None, None, Some(&cookie))
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {body}");
        let items = body["items"].as_array().unwrap();
        let transitions = [
            "publish",
            "withdraw",
            "restore",
            "takedown",
            "admin_restore",
        ];
        let actions: Vec<&str> = items
            .iter()
            .map(|item| item["action"].as_str().unwrap())
            .filter(|action| transitions.contains(action))
            .collect();
        // Newest first, so the admin restore — the last thing that happened —
        // comes back on top.
        assert_eq!(
            actions,
            ["admin_restore", "takedown", "restore", "withdraw", "publish"],
            "{kind:?}"
        );

        for item in items.iter().filter(|item| item["slug"] == "alpha") {
            assert_eq!(item["kind"], kind.audit_kind(), "{kind:?}");
            assert!(item["at"].as_i64().unwrap() > 0, "{kind:?}");
            assert!(
                item["actor"].as_str().is_some_and(|a| a.contains(':')),
                "{kind:?} actor: {}",
                item["actor"]
            );
        }
        // The publish entry carries the version, the withdraw entry the reason.
        let publish = items.iter().find(|i| i["action"] == "publish").unwrap();
        assert_eq!(publish["version"], "1.0.0", "{kind:?}");
        let withdraw = items.iter().find(|i| i["action"] == "withdraw").unwrap();
        assert_eq!(withdraw["detail"], "retired", "{kind:?}");

        // The kind filter really filters.
        let other = match kind {
            Kind::Skill => "agent",
            Kind::Agent => "skill",
        };
        let (_, filtered, _) = h
            .call("GET", &format!("/admin/api/audit?kind={other}"), None, None, Some(&cookie))
            .await;
        let foreign: Vec<&Value> = filtered["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["slug"] == "alpha")
            .collect();
        assert_eq!(foreign.len(), 0, "{kind:?} audit kind filter: {filtered}");
    }
}

/// The sign-in form is the only unauthenticated way in, and each attempt costs
/// an Argon2 verification and an audit row. Budget it.
#[tokio::test]
async fn failed_sign_ins_are_rate_limited() {
    let h = Harness::new();
    async fn attempt(h: &Harness, username: &str, password: &str) -> (StatusCode, Value, Option<String>) {
        let body = json!({ "username": username, "password": password });
        h.call("POST", "/admin/api/login", None, Some(body), None).await
    }
    for _ in 0..5 {
        let (status, answer, cookie) = attempt(&h, ADMIN_USER, "wrong").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{answer}");
        assert!(cookie.is_none(), "a failed sign-in handed out a session");
    }

    // The budget is spent: even the right password is refused now.
    let (status, answer, cookie) = attempt(&h, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{answer}");
    assert_eq!(answer["error"]["code"], "too_many_attempts", "{answer}");
    assert!(cookie.is_none(), "a locked-out sign-in handed out a session");

    // An oversized field is refused before the password is hashed, so it costs
    // nothing and leaves no trail beyond the five misses above.
    let huge = "x".repeat(4096);
    let (status, answer, _) = attempt(&h, ADMIN_USER, &huge).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");

    // Inventing usernames does not buy unlimited attempts: the shared budget
    // runs out too.
    for index in 0..15 {
        attempt(&h, &format!("nobody-{index}"), "wrong").await;
    }
    let (status, answer, _) = attempt(&h, ADMIN_USER, ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{answer}");

    // Bounded trail: the budget is what stops a flood from filling the table,
    // so count the rows the flood actually produced.
    let counts: (i64, i64) = {
        let db = h.db();
        let count = |action: &str| -> i64 {
            db.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = ?1",
                [action],
                |row| row.get(0),
            )
            .unwrap()
        };
        (count("admin_login_failed"), count("admin_login_locked"))
    };
    assert_eq!(counts.0, 20, "one row per miss, no more");
    // Two markers: the per-username budget at the 5th miss, the shared one at
    // the 20th. Never one per miss.
    assert_eq!(counts.1, 2, "lock markers: {counts:?}");
}

#[tokio::test]
async fn sign_in_events_belong_to_no_listing() {
    let h = Harness::new();
    h.publish_skill("tenant-a", "alpha", "1.0.0", "public").await;

    // A failed sign-in, then a real one that we end again.
    let (failed, _, _) = h
        .call(
            "POST",
            "/admin/api/login",
            None,
            Some(json!({ "username": ADMIN_USER, "password": "wrong" })),
            None,
        )
        .await;
    assert_eq!(failed, StatusCode::UNAUTHORIZED);
    let cookie = h.login().await;
    let (out, _, _) = h
        .call("POST", "/admin/api/logout", None, None, Some(&cookie))
        .await;
    assert_eq!(out, StatusCode::OK);

    // Unfiltered, everything is there.
    let cookie = h.login().await;
    let (_, all, _) = h
        .call("GET", "/admin/api/audit", None, None, Some(&cookie))
        .await;
    let events: Vec<&Value> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| {
            matches!(
                item["action"].as_str().unwrap(),
                "admin_login" | "admin_login_failed" | "admin_logout"
            )
        })
        .collect();
    // Failed attempt, the sign-in that worked, the sign-out, then the second
    // sign-in this test needs to read the trail — newest first.
    let trail: Vec<&str> = events
        .iter()
        .map(|event| event["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        trail,
        ["admin_login", "admin_logout", "admin_login", "admin_login_failed"],
        "{all}"
    );
    for event in &events {
        assert_eq!(event["kind"], "admin", "{} is not tagged admin", event["action"]);
        assert!(event["slug"].is_null(), "{} claims a listing", event["action"]);
    }

    // The admin feed is exactly the sign-in trail, and it stays out of the
    // skill feed — otherwise a listing's history looks like it was logged in.
    let (_, admin, _) = h
        .call("GET", "/admin/api/audit?kind=admin", None, None, Some(&cookie))
        .await;
    assert!(admin["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["kind"] == "admin"), "{admin}");
    let (_, skills, _) = h
        .call("GET", "/admin/api/audit?kind=skill", None, None, Some(&cookie))
        .await;
    let signins = skills["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["action"].as_str().unwrap().starts_with("admin_"))
        .count();
    assert_eq!(signins, 0, "sign-in events leaked into the skill feed: {skills}");
    assert!(skills["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["action"] == "publish"), "{skills}");
}

// ---------------------------------------------------------------------------
// request guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn writes_are_guarded_like_reads() {
    for kind in Kind::ALL {
        let h = Harness::new();
        kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;

        // No tenant header on any write route: refused before touching a row.
        let no_tenant = [
            ("POST", kind.list().to_string()),
            ("POST", kind.action("alpha", "withdraw")),
            ("POST", kind.action("alpha", "restore")),
        ];
        for (method, uri) in no_tenant {
            let (status, body, _) = h.call(method, &uri, None, Some(json!({})), None).await;
            assert!(
                status == StatusCode::UNAUTHORIZED || status == StatusCode::BAD_REQUEST,
                "{kind:?} {uri} without tenant: {status} {body}"
            );
        }
        // ... and the entry is untouched.
        let (_, status_body) = kind.status_of(&h, "tenant-b", "alpha").await;
        assert_eq!(status_body["state"], "active", "{kind:?}");

        // Admin state changes need a session, not just a tenant header.
        for action in ["takedown", "restore"] {
            let (status, _, _) = h
                .call(
                    "POST",
                    &kind.admin_action("alpha", action),
                    Some("tenant-a"),
                    Some(json!({})),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{kind:?} {action}");
        }
    }
}

/// Both planes answer a dry run, and neither writes anything: the publish form
/// calls this on the archive the user picked, so a "no" must not leave a row.
#[tokio::test]
async fn validate_answers_without_writing_anything() {
    for kind in Kind::ALL {
        let h = Harness::new();
        let files = kind.files("check me");
        let body = match kind {
            Kind::Skill => json!({
                "name": "Demo Skill", "slug": "alpha", "version": "1.0.0",
                "visibility": "public", "files": files
            }),
            Kind::Agent => json!({
                "slug": "alpha", "version": "1.0.0", "visibility": "public",
                "files": files
            }),
        };
        let uri = format!("{}/validate", kind.list());
        let (status, answer, _) = h
            .call("POST", &uri, Some("tenant-a"), Some(body), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {answer}");
        assert_eq!(answer["ok"], true, "{kind:?}: {answer}");
        assert_eq!(answer["problems"].as_array().unwrap().len(), 0, "{kind:?}");
        // `existing` describes whatever already holds the slug; null when free.
        assert!(answer["existing"].is_null(), "{kind:?}: {answer}");

        // Validation is a dry run: the market is still empty.
        let (_, listing, _) = h
            .call("GET", kind.list(), Some("tenant-a"), None, None)
            .await;
        assert_eq!(listing["total"], 0, "{kind:?} validate published something");

        // Once the slug exists the same payload is refused, with the reason.
        kind.publish(&h, "tenant-a", "alpha", "1.0.0", "public").await;
        let body = match kind {
            Kind::Skill => json!({
                "name": "Demo Skill", "slug": "alpha", "version": "1.0.0",
                "visibility": "public", "files": kind.files("check me")
            }),
            Kind::Agent => json!({
                "slug": "alpha", "version": "1.0.0", "visibility": "public",
                "files": kind.files("check me")
            }),
        };
        let (status, answer, _) = h
            .call("POST", &uri, Some("tenant-a"), Some(body), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{kind:?}: {answer}");
        assert_eq!(answer["ok"], false, "{kind:?}");
        let existing = &answer["existing"];
        assert_eq!(existing["mine"], true, "{kind:?}: {existing}");
        assert_eq!(existing["state"], "active", "{kind:?}: {existing}");
        assert_eq!(existing["latest_version"], "1.0.0", "{kind:?}: {existing}");
        assert_eq!(existing["owner_tenant"], "tenant-a", "{kind:?}: {existing}");
        let codes: Vec<&str> = answer["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["code"].as_str().unwrap())
            .collect();
        assert!(codes.contains(&"version_exists"), "{kind:?}: {codes:?}");
    }
}
