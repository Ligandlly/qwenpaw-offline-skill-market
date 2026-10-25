//! Integration tests for the M1 surface. These encode the PLAN §1 decisions:
//! fail-closed isolation, immutable versions, withdraw vs. takedown semantics.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use offline_market_backend::{
    build_state,
    config::{Config, Limits},
    db, router,
};
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    app: Router,
}

const GOOD_MANIFEST: &str = "---\nname: demo\ndescription: a demo skill\n---\n\n# hello\n";

fn test_config() -> Config {
    Config {
        bind: "127.0.0.1:0".into(),
        db_path: "unused".into(),
        public_url: "http://market.test".into(),
        admin_user: "admin".into(),
        admin_password: "s3cret-pw".into(),
        log_level: "warn".into(),
        limits: Limits {
            max_bundle_bytes: 2 * 1024 * 1024,
            max_files: 100,
            max_file_bytes: 256 * 1024,
        },
    }
}

impl Harness {
    fn new() -> Self {
        let cfg = test_config();
        let conn = db::open_memory().expect("in-memory db");
        let state = build_state(cfg, conn);
        offline_market_backend::admin::ensure_admin(&state);
        Self { app: router(state) }
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        tenant: Option<&str>,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> (StatusCode, Value, Option<String>) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(tenant) = tenant {
            builder = builder.header("x-tenant-id", tenant);
        }
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let set_cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or_default().to_string());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
        (status, value, set_cookie)
    }

    async fn publish(
        &self,
        tenant: &str,
        slug: &str,
        version: &str,
        visibility: &str,
    ) -> (StatusCode, Value) {
        let (status, body, _) = self
            .call(
                "POST",
                "/api/v1/skills",
                Some(tenant),
                Some(json!({
                    "name": format!("Skill {slug}"),
                    "slug": slug,
                    "version": version,
                    "visibility": visibility,
                    "files": { "SKILL.md": GOOD_MANIFEST }
                })),
                None,
            )
            .await;
        (status, body)
    }

    async fn login(&self) -> String {
        let (status, _body, cookie) = self
            .call(
                "POST",
                "/admin/api/login",
                None,
                Some(json!({ "username": "admin", "password": "s3cret-pw" })),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "admin login should succeed");
        cookie.expect("session cookie")
    }
}

#[tokio::test]
async fn tenant_header_is_required() {
    let h = Harness::new();
    let (status, body, _) = h.call("GET", "/api/v1/skills", None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "missing_tenant");
}

#[tokio::test]
async fn publish_list_and_download_roundtrip() {
    let h = Harness::new();
    let (status, body) = h.publish("tenant-a", "demo", "1.0.0", "public").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["install_url"], "http://market.test/skills/demo/1.0.0");
    assert_eq!(body["file_count"], 1);

    // another tenant sees it in the market
    let (status, listing, _) = h
        .call("GET", "/api/v1/skills?q=demo", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["items"][0]["slug"], "demo");

    // and can download through the short alias used as bundle_url
    let (status, bundle, _) = h
        .call("GET", "/skills/demo/1.0.0", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bundle["name"], "Skill demo");
    assert!(bundle["files"]["SKILL.md"]
        .as_str()
        .unwrap()
        .contains("description: a demo skill"));

    // "latest" resolves through the pointer
    let (status, bundle, _) = h
        .call("GET", "/skills/demo/latest", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bundle["version"], "1.0.0");
}

#[tokio::test]
async fn versions_are_immutable_and_latest_only_moves_forward() {
    let h = Harness::new();
    h.publish("tenant-a", "demo", "1.0.0", "public").await;
    h.publish("tenant-a", "demo", "2.0.0", "public").await;

    let (status, body) = h.publish("tenant-a", "demo", "1.0.0", "public").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "version_exists");

    // publishing an *older* version must not roll `latest` back
    let (status, body) = h.publish("tenant-a", "demo", "1.5.0", "public").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["latest_version"], "2.0.0");
}

#[tokio::test]
async fn private_skills_are_fail_closed() {
    let h = Harness::new();
    h.publish("tenant-a", "secret", "1.0.0", "tenant").await;

    let (status, body, _) = h
        .call("GET", "/skills/secret/1.0.0", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "forbidden");

    let (_s, listing, _) = h
        .call("GET", "/api/v1/skills", Some("tenant-b"), None, None)
        .await;
    assert!(
        !listing["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["slug"] == "secret"),
        "private skill leaked into another tenant's listing"
    );

    // owner still gets it
    let (status, _, _) = h
        .call("GET", "/skills/secret/1.0.0", Some("tenant-a"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);

    // and status is hidden from strangers too
    let (status, _, _) = h
        .call(
            "GET",
            "/api/v1/skills/secret/status",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deep_paths_publish_without_a_depth_gate() {
    let h = Harness::new();
    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/skills",
            Some("tenant-a"),
            Some(json!({
                "name": "deep", "slug": "deep", "version": "1.0.0", "visibility": "public",
                "files": {
                    "SKILL.md": GOOD_MANIFEST,
                    "scripts/office/helpers/__init__.py": "",
                }
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["file_count"], 2);
}

#[tokio::test]
async fn gate_rejects_traversal_and_missing_manifest() {
    let h = Harness::new();
    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/skills",
            Some("tenant-a"),
            Some(json!({
                "name": "evil", "slug": "evil", "version": "1.0.0", "visibility": "public",
                "files": { "SKILL.md": GOOD_MANIFEST, "../../etc/passwd": "boom" }
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(matches!(
        body["error"]["code"].as_str(),
        Some("traversal" | "invalid_path")
    ));

    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/skills",
            Some("tenant-a"),
            Some(json!({
                "name": "nomanifest", "slug": "nomanifest", "version": "1.0.0", "visibility": "public",
                "files": { "readme.md": "x" }
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "missing_manifest");
}

#[tokio::test]
async fn withdraw_hides_from_market_but_not_from_owner() {
    let h = Harness::new();
    h.publish("tenant-a", "demo", "1.0.0", "public").await;

    let (status, _, _) = h
        .call(
            "POST",
            "/api/v1/skills/demo/withdraw",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = h
        .call("GET", "/skills/demo/1.0.0", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::GONE);

    let (status, _, _) = h
        .call("GET", "/skills/demo/1.0.0", Some("tenant-a"), None, None)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "owner keeps access to its own withdrawn skill"
    );

    let (_s, listing, _) = h
        .call("GET", "/api/v1/skills", Some("tenant-b"), None, None)
        .await;
    assert!(!listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["slug"] == "demo"));

    // reversible
    let (status, _, _) = h
        .call(
            "POST",
            "/api/v1/skills/demo/restore",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = h
        .call("GET", "/skills/demo/1.0.0", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);

    // a stranger cannot withdraw someone else's skill
    let (status, _, _) = h
        .call(
            "POST",
            "/api/v1/skills/demo/withdraw",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_takedown_stops_redistribution_and_blocks_self_restore() {
    let h = Harness::new();
    h.publish("tenant-a", "bad", "1.0.0", "public").await;
    let cookie = h.login().await;

    // no cookie -> rejected
    let (status, _, _) = h
        .call(
            "POST",
            "/admin/api/skills/bad/takedown",
            None,
            Some(json!({"reason":"malware"})),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = h
        .call(
            "POST",
            "/admin/api/skills/bad/takedown",
            None,
            Some(json!({"reason": "malware"})),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // whole package is gone for everyone, including the owner
    for tenant in ["tenant-a", "tenant-b"] {
        let (status, body, _) = h
            .call("GET", "/skills/bad/1.0.0", Some(tenant), None, None)
            .await;
        assert_eq!(status, StatusCode::GONE, "{tenant}: {body}");
    }
    let (_s, listing, _) = h
        .call("GET", "/api/v1/skills", Some("tenant-b"), None, None)
        .await;
    assert!(!listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["slug"] == "bad"));

    // owner cannot self-restore, cannot publish over it
    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/skills/bad/restore",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "taken_down");

    let (status, body) = h.publish("tenant-a", "bad", "1.0.1", "public").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "taken_down");

    // admin can restore
    let (status, _, _) = h
        .call(
            "POST",
            "/admin/api/skills/bad/restore",
            None,
            None,
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = h
        .call("GET", "/skills/bad/1.0.0", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn slug_ownership_cannot_be_hijacked() {
    let h = Harness::new();
    h.publish("tenant-a", "taken", "1.0.0", "public").await;
    let (status, body) = h.publish("tenant-b", "taken", "2.0.0", "public").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn admin_login_and_audit_trail() {
    let h = Harness::new();
    h.publish("tenant-a", "demo", "1.0.0", "public").await;

    let (status, body, _) = h
        .call(
            "POST",
            "/admin/api/login",
            None,
            Some(json!({ "username": "admin", "password": "wrong" })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "invalid_credentials");

    let cookie = h.login().await;
    let (status, listing, _) = h
        .call("GET", "/admin/api/skills", None, None, Some(&cookie))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["items"][0]["slug"], "demo");

    let (status, audit, _) = h
        .call("GET", "/admin/api/audit", None, None, Some(&cookie))
        .await;
    assert_eq!(status, StatusCode::OK);
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["action"].as_str())
        .collect();
    assert!(actions.contains(&"publish"), "audit: {actions:?}");
    assert!(actions.contains(&"admin_login"), "audit: {actions:?}");
    assert!(
        actions.contains(&"admin_login_failed"),
        "audit: {actions:?}"
    );

    // logout invalidates the session
    let (status, _, _) = h
        .call("POST", "/admin/api/logout", None, None, Some(&cookie))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = h
        .call("GET", "/admin/api/skills", None, None, Some(&cookie))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn limits_are_exposed_for_the_plugin_to_mirror() {
    let h = Harness::new();
    let (status, body, _) = h
        .call("GET", "/api/v1/limits", Some("tenant-a"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["max_files"], 100);
    assert_eq!(body["manifest"], "SKILL.md");
}

#[tokio::test]
async fn lookup_maps_display_name_to_slug_even_after_takedown() {
    let h = Harness::new();
    h.publish("tenant-a", "demo", "1.0.0", "public").await;
    h.publish("tenant-a", "secret", "1.0.0", "tenant").await;
    let cookie = h.login().await;
    h.call(
        "POST",
        "/admin/api/skills/demo/takedown",
        None,
        Some(json!({"reason": "test"})),
        Some(&cookie),
    )
    .await;

    // the host reports the display name, we answer with the slug + state
    let (status, body, _) = h
        .call(
            "GET",
            "/api/v1/skills/lookup?names=Skill%20demo,Skill%20secret,Skill%20missing",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{body}"); // only what tenant-a may see
    let demo = items.iter().find(|i| i["slug"] == "demo").unwrap();
    assert_eq!(demo["state"], "taken_down");
    assert_eq!(demo["takedown_reason"], "test");
    assert_eq!(demo["install_url"], "http://market.test/skills/demo/1.0.0");

    // a different tenant must not see tenant-a's private skill through lookup
    let (status, body, _) = h
        .call(
            "GET",
            "/api/v1/skills/lookup?names=Skill%20secret",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
}
