//! Integration tests for the agent market (MVP). They encode the same
//! semantics as the skill market: immutable versions, fail-closed private
//! packages, reversible owner withdrawal, and admin-only takedown.

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
    state: offline_market_backend::state::Shared,
}

const GOOD_SKILL: &str = "---\nname: demo-skill\ndescription: a demo skill\n---\n\n# hello\n";

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

fn agent_files(name: &str, description: &str) -> Value {
    let manifest = json!({
        "schema": 1,
        "name": name,
        "description": description,
        "language": "zh",
        "profile": {
            "thinking_level": "medium",
            "plan_enabled": false,
            "disabled_tools": ["browser"]
        },
        "md_files": ["SOUL.md", "AGENTS.md"],
        "skills": ["demo-skill"]
    })
    .to_string();
    json!({
        "manifest.json": manifest,
        "SOUL.md": "# soul\n",
        "AGENTS.md": "# agents\n",
        "skills/demo-skill/SKILL.md": GOOD_SKILL,
        "skills/demo-skill/scripts/run.py": "print(1)\n"
    })
}

impl Harness {
    fn new() -> Self {
        let cfg = test_config();
        let conn = db::open_memory().expect("in-memory db");
        let state = build_state(cfg, conn);
        offline_market_backend::admin::ensure_admin(&state);
        Self {
            app: router(state.clone()),
            state,
        }
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
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or_default().to_string());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
        (status, value, set_cookie)
    }

    async fn publish_agent(
        &self,
        tenant: &str,
        slug: &str,
        version: &str,
        visibility: &str,
    ) -> (StatusCode, Value) {
        let (status, body, _) = self
            .call(
                "POST",
                "/api/v1/agents",
                Some(tenant),
                Some(json!({
                    "slug": slug,
                    "version": version,
                    "visibility": visibility,
                    "publisher_name": "张三",
                    "files": agent_files("QA Agent", "面向配置问答的智能体")
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
async fn agent_publish_list_and_download_roundtrip() {
    let h = Harness::new();
    let (status, body) = h
        .publish_agent("tenant-a", "qa-agent", "1.0.0", "public")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["skill_count"], 1);
    assert_eq!(body["file_count"], 5);

    let (status, listing, _) = h
        .call("GET", "/api/v1/agents", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    let item = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["slug"] == "qa-agent")
        .expect("listing item")
        .clone();
    assert_eq!(item["name"], "QA Agent");
    assert_eq!(item["description"], "面向配置问答的智能体");
    assert_eq!(item["skill_count"], 1);
    assert_eq!(item["owner_name"], "张三");

    let (status, bundle, _) = h
        .call(
            "GET",
            "/api/v1/agents/qa-agent/latest/bundle",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{bundle}");
    assert_eq!(bundle["name"], "QA Agent");
    assert_eq!(bundle["version"], "1.0.0");
    assert_eq!(bundle["files"]["SOUL.md"], "# soul\n");
    assert_eq!(bundle["files"]["skills/demo-skill/SKILL.md"], GOOD_SKILL);
    assert!(bundle["source_url"]
        .as_str()
        .unwrap()
        .ends_with("/api/v1/agents/qa-agent/1.0.0/bundle"));
}

#[tokio::test]
async fn agent_skill_names_are_not_style_restricted() {
    let h = Harness::new();
    let files = json!({
        "manifest.json": json!({
            "schema": 1,
            "name": "QA Agent",
            "description": "bundles existing host skills",
            "language": "zh",
            "profile": {"thinking_level": "inherit"},
            "md_files": ["SOUL.md"],
            "skills": ["QA_source_index", "周报助手"]
        })
        .to_string(),
        "SOUL.md": "# soul\n",
        "skills/QA_source_index/SKILL.md":
            "---\nname: QA_source_index\ndescription: uppercase\n---\n",
        "skills/周报助手/SKILL.md":
            "---\nname: 周报助手\ndescription: 中文技能\n---\n",
    });
    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/agents",
            Some("tenant-a"),
            Some(json!({
                "slug": "style-free-agent",
                "version": "1.0.0",
                "visibility": "public",
                "files": files
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["skill_count"], 2);

    let (status, bundle, _) = h
        .call(
            "GET",
            "/api/v1/agents/style-free-agent/latest/bundle",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{bundle}");
    assert!(bundle["files"]["skills/QA_source_index/SKILL.md"].is_string());
    assert!(bundle["files"]["skills/周报助手/SKILL.md"].is_string());

    for (name, code) in [
        ("a/b", "invalid_skill_name"),
        ("a\\b", "invalid_skill_name"),
        (".", "invalid_skill_name"),
        ("..", "invalid_skill_name"),
        (" leading", "invalid_skill_name"),
        ("trailing ", "invalid_skill_name"),
    ] {
        let bad = json!({
            "manifest.json": json!({
                "schema": 1,
                "name": "QA Agent",
                "language": "zh",
                "md_files": ["SOUL.md"],
                "skills": [name]
            })
            .to_string(),
            "SOUL.md": "# soul\n",
            (format!("skills/{name}/SKILL.md")): "---\nname: demo\ndescription: demo\n---\n",
        });
        let (status, body, _) = h
            .call(
                "POST",
                "/api/v1/agents",
                Some("tenant-a"),
                Some(json!({
                    "slug": "bad-skill-agent",
                    "version": "1.0.0",
                    "visibility": "public",
                    "files": bad
                })),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert_eq!(body["error"]["code"], code, "{name}: {body}");
    }
}

#[tokio::test]
async fn agent_names_without_ascii_get_a_stable_slug() {
    let h = Harness::new();
    let files = agent_files("周报助手", "中文名智能体");
    let (status, first, _) = h
        .call(
            "POST",
            "/api/v1/agents",
            Some("tenant-a"),
            Some(json!({
                "version": "1.0.0",
                "visibility": "public",
                "files": files
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let slug = first["slug"].as_str().expect("slug").to_string();
    assert!(slug.starts_with("agent-"), "{slug}");
    assert!(offline_market_backend::limits::valid_slug(&slug), "{slug}");

    let (status, second, _) = h
        .call(
            "POST",
            "/api/v1/agents",
            Some("tenant-a"),
            Some(json!({
                "version": "1.0.1",
                "visibility": "public",
                "files": agent_files("周报助手", "中文名智能体")
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    assert_eq!(second["slug"], slug.as_str());
    assert_eq!(second["latest_version"], "1.0.1");
}

#[tokio::test]
async fn agent_versions_are_immutable_and_latest_only_moves_forward() {
    let h = Harness::new();
    for version in ["1.0.0", "1.0.2"] {
        let (status, body) = h
            .publish_agent("tenant-a", "qa-agent", version, "public")
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let (status, body) = h
        .publish_agent("tenant-a", "qa-agent", "1.0.1", "public")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["latest_version"], "1.0.2");

    let (status, body) = h
        .publish_agent("tenant-a", "qa-agent", "1.0.0", "public")
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "version_exists");

    let (_, bundle, _) = h
        .call(
            "GET",
            "/api/v1/agents/qa-agent/latest/bundle",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(bundle["version"], "1.0.2");
}

#[tokio::test]
async fn private_agents_are_fail_closed() {
    let h = Harness::new();
    let (status, body) = h
        .publish_agent("tenant-a", "private-agent", "1.0.0", "tenant")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (_, listing, _) = h
        .call("GET", "/api/v1/agents", Some("tenant-b"), None, None)
        .await;
    assert!(listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["slug"] != "private-agent"));

    let (status, body, _) = h
        .call(
            "GET",
            "/api/v1/agents/private-agent/latest/bundle",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body, _) = h
        .call(
            "GET",
            "/api/v1/agents/private-agent/status",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, _, _) = h
        .call(
            "GET",
            "/api/v1/agents/private-agent/latest/bundle",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn agent_withdraw_hides_but_owner_can_still_fetch() {
    let h = Harness::new();
    h.publish_agent("tenant-a", "qa-agent", "1.0.0", "public")
        .await;

    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/agents/qa-agent/withdraw",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/agents/qa-agent/withdraw",
            Some("tenant-a"),
            Some(json!({ "reason": "临时下线" })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "withdrawn");

    let (_, listing, _) = h
        .call("GET", "/api/v1/agents", Some("tenant-b"), None, None)
        .await;
    assert!(listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["slug"] != "qa-agent"));

    let (status, body, _) = h
        .call(
            "GET",
            "/api/v1/agents/qa-agent/latest/bundle",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["error"]["code"], "withdrawn");

    let (status, _, _) = h
        .call(
            "GET",
            "/api/v1/agents/qa-agent/latest/bundle",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/agents/qa-agent/restore",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "active");
}

#[tokio::test]
async fn admin_agent_takedown_blocks_redistribution_and_self_restore() {
    let h = Harness::new();
    h.publish_agent("tenant-a", "qa-agent", "1.0.0", "public")
        .await;
    let cookie = h.login().await;

    let (status, body, _) = h
        .call(
            "POST",
            "/admin/api/agents/qa-agent/takedown",
            None,
            Some(json!({ "reason": "违规" })),
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "taken_down");

    let (_, listing, _) = h
        .call("GET", "/api/v1/agents", Some("tenant-b"), None, None)
        .await;
    assert!(listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["slug"] != "qa-agent"));

    let (status, body, _) = h
        .call(
            "GET",
            "/api/v1/agents/qa-agent/latest/bundle",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["error"]["code"], "taken_down");

    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/agents/qa-agent/restore",
            Some("tenant-a"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, body) = h
        .publish_agent("tenant-a", "qa-agent", "2.0.0", "public")
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "taken_down");

    let (status, _body, _) = h
        .call(
            "POST",
            "/admin/api/agents/qa-agent/restore",
            None,
            None,
            Some(&cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _bundle, _) = h
        .call(
            "GET",
            "/api/v1/agents/qa-agent/latest/bundle",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, audit, _) = h
        .call(
            "GET",
            "/admin/api/audit?kind=agent&limit=50",
            None,
            None,
            Some(&cookie),
        )
        .await;
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["action"].as_str().unwrap())
        .collect();
    assert!(actions.contains(&"takedown"));
    assert!(actions.contains(&"admin_restore"));
    assert!(audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["kind"] == "agent"));
}

#[tokio::test]
async fn agent_gate_rejects_bad_bundles() {
    let h = Harness::new();

    // Root file that is not declared as an md_file.
    let mut files = agent_files("QA Agent", "demo");
    files["MEMORY.md"] = json!("private memory");
    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/agents",
            Some("tenant-a"),
            Some(json!({
                "slug": "bad-agent", "version": "1.0.0", "visibility": "public",
                "files": files
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "unexpected_root_file");

    // A skill file whose directory was never declared.
    let mut files = agent_files("QA Agent", "demo");
    files["skills/other/run.py"] = json!("print(1)");
    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/agents",
            Some("tenant-a"),
            Some(json!({
                "slug": "bad-agent", "version": "1.0.0", "visibility": "public",
                "files": files
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "undeclared_skill");

    // Missing manifest.
    let (status, body, _) = h
        .call(
            "POST",
            "/api/v1/agents",
            Some("tenant-a"),
            Some(json!({
                "slug": "bad-agent", "version": "1.0.0", "visibility": "public",
                "files": { "SOUL.md": "# soul" }
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "missing_manifest");
}

#[tokio::test]
async fn agent_slug_ownership_cannot_be_hijacked() {
    let h = Harness::new();
    h.publish_agent("tenant-a", "qa-agent", "1.0.0", "public")
        .await;

    let (status, body) = h
        .publish_agent("tenant-b", "qa-agent", "2.0.0", "public")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn agent_status_reports_market_state() {
    let h = Harness::new();
    h.publish_agent("tenant-a", "qa-agent", "1.0.0", "public")
        .await;

    let (status, body, _) = h
        .call(
            "GET",
            "/api/v1/agents/qa-agent/status",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "active");
    assert_eq!(body["latest_version"], "1.0.0");
}

#[tokio::test]
async fn dashboard_counts_agents_and_their_downloads() {
    let h = Harness::new();
    h.publish_agent("tenant-a", "qa-agent", "1.0.0", "public")
        .await;
    for _ in 0..3 {
        let (status, _, _) = h
            .call(
                "GET",
                "/api/v1/agents/qa-agent/latest/bundle",
                Some("tenant-b"),
                None,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, stats, _) = h
        .call("GET", "/api/v1/stats", Some("tenant-b"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["agents"]["total"], 1);
    assert_eq!(stats["agents"]["new_this_month"], 1);
    assert_eq!(stats["agents"]["downloads"], 3);
    assert_eq!(stats["skills"]["total"], 0);
    assert_eq!(stats["totals"]["agents"], 1);
    assert_eq!(stats["totals"]["downloads"], 3);
}

#[tokio::test]
async fn agent_listing_reports_totals_and_whitelisted_sort_orders() {
    let h = Harness::new();
    for slug in ["alpha-agent", "beta-agent", "gamma-agent"] {
        let (status, body) = h.publish_agent("tenant-a", slug, "1.0.0", "public").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    for _ in 0..2 {
        let (status, _, _) = h
            .call(
                "GET",
                "/api/v1/agents/beta-agent/latest/bundle",
                Some("tenant-b"),
                None,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _, _) = h
        .call(
            "GET",
            "/api/v1/agents/gamma-agent/latest/bundle",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, listing, _) = h
        .call(
            "GET",
            "/api/v1/agents?limit=1&sort=downloads",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(listing["total"], 3);
    assert_eq!(listing["sort"], "downloads");
    assert_eq!(listing["items"].as_array().unwrap().len(), 1);
    assert_eq!(listing["items"][0]["slug"], "beta-agent");
    assert_eq!(listing["items"][0]["download_count"], 2);

    let (_, second, _) = h
        .call(
            "GET",
            "/api/v1/agents?limit=1&page=2&sort=downloads",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(second["items"][0]["slug"], "gamma-agent");
    assert_eq!(second["items"][0]["download_count"], 1);

    h.state
        .db()
        .execute(
            "UPDATE agent_versions SET created_at = 4102444800 WHERE slug = 'alpha-agent'",
            [],
        )
        .unwrap();
    let (_, updated, _) = h
        .call(
            "GET",
            "/api/v1/agents?sort=updated&limit=50",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(updated["sort"], "updated");
    assert_eq!(updated["items"][0]["slug"], "alpha-agent");

    let (_, fallback, _) = h
        .call(
            "GET",
            "/api/v1/agents?sort=DROP%20TABLE",
            Some("tenant-b"),
            None,
            None,
        )
        .await;
    assert_eq!(fallback["total"], 3);
    assert_eq!(fallback["sort"], "created");
}
