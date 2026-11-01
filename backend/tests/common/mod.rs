//! Shared integration-test harness.
//!
//! `tests/api.rs` (skill plane) and `tests/agents.rs` (agent plane) both drive
//! the real router through `tower::ServiceExt::oneshot` against an in-memory
//! SQLite database. The two planes are supposed to be the same market with a
//! different object type, so the fixtures that build a request live here once:
//! a contract that holds in only one plane is a bug, and single-sourcing the
//! harness is what makes that claim checkable.

#![allow(dead_code)]

use std::sync::MutexGuard;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use offline_market_backend::{
    build_state,
    config::{Config, Limits},
    db, router,
    state::Shared,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use tower::ServiceExt;

pub const ADMIN_USER: &str = "admin";
pub const ADMIN_PASSWORD: &str = "s3cret-pw";

/// A minimal but *valid* `SKILL.md` manifest: file name + frontmatter name must
/// both parse, otherwise `check_bundle` rejects the publish.
pub const GOOD_MANIFEST: &str =
    "---\nname: demo\ndescription: a demo skill\n---\n\n# hello\n";

/// The same manifest under a different `name`, for bundles that carry skills.
pub const GOOD_SKILL: &str =
    "---\nname: demo-skill\ndescription: a demo skill\n---\n\n# hello\n";

pub fn test_config() -> Config {
    Config {
        bind: "127.0.0.1:0".into(),
        db_path: "unused".into(),
        public_url: "http://market.test".into(),
        admin_user: ADMIN_USER.into(),
        admin_password: ADMIN_PASSWORD.into(),
        log_level: "warn".into(),
        request_timeout: Duration::from_secs(30),
        limits: Limits {
            max_bundle_bytes: 2 * 1024 * 1024,
            max_files: 100,
            max_file_bytes: 256 * 1024,
        },
    }
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// Files of a publishable skill, single `SKILL.md`.
pub fn skill_files() -> Value {
    json!({ "SKILL.md": GOOD_MANIFEST })
}

/// Files of a publishable agent: `manifest.json` plus the declared markdown.
pub fn agent_files(name: &str, description: &str) -> Value {
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

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

pub struct Harness {
    pub app: Router,
    pub state: Shared,
}

impl Harness {
    pub fn new() -> Self {
        Self::with_config(test_config())
    }

    pub fn with_config(cfg: Config) -> Self {
        let conn = db::open_memory().expect("in-memory db");
        let state = build_state(cfg, conn);
        offline_market_backend::admin::ensure_admin(&state);
        Self {
            app: router(state.clone()),
            state,
        }
    }

    /// Raw access to the test database: fixtures write rows directly when a
    /// test needs a state the HTTP API cannot produce (a backdated version, a
    /// dropped table, a failing audit trigger).
    pub fn db(&self) -> MutexGuard<'_, Connection> {
        self.state.db()
    }

    pub fn exec(&self, sql: &str) {
        self.db().execute_batch(sql).expect("fixture sql");
    }

    pub async fn call(
        &self,
        method: &str,
        uri: &str,
        tenant: Option<&str>,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> (StatusCode, Value, Option<String>) {
        let (status, _headers, bytes, set_cookie) = self
            .call_raw(method, uri, tenant, body.map(|v| v.to_string()), cookie)
            .await;
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }));
        (status, value, set_cookie)
    }

    /// The same request, but carrying a body the JSON helper cannot express
    /// (invalid JSON, an oversized payload) and returning the raw bytes —
    /// needed whenever the *transport* behaviour is the thing under test.
    pub async fn call_raw(
        &self,
        method: &str,
        uri: &str,
        tenant: Option<&str>,
        body: Option<String>,
        cookie: Option<&str>,
    ) -> (StatusCode, HeaderMap, Bytes, Option<String>) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(tenant) = tenant {
            builder = builder.header("x-tenant-id", tenant);
        }
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        let request = match body {
            Some(raw) => builder
                .header("content-type", "application/json")
                .body(Body::from(raw))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let set_cookie = headers
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or_default().to_string());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, bytes, set_cookie)
    }

    /// GET a bundle and return its body bytes untouched (the hub hands these
    /// to the host, so byte-level identity is part of the contract).
    pub async fn download(
        &self,
        uri: &str,
        tenant: Option<&str>,
    ) -> (StatusCode, Bytes) {
        let (status, _headers, bytes, _) = self.call_raw("GET", uri, tenant, None, None).await;
        (status, bytes)
    }

    pub async fn publish_skill(
        &self,
        tenant: &str,
        slug: &str,
        version: &str,
        visibility: &str,
    ) -> (StatusCode, Value) {
        self.publish_skill_with(
            tenant,
            json!({
                "name": format!("Skill {slug}"),
                "slug": slug,
                "version": version,
                "visibility": visibility,
                "files": skill_files()
            }),
        )
        .await
    }

    pub async fn publish_skill_with(&self, tenant: &str, body: Value) -> (StatusCode, Value) {
        let (status, body, _) = self
            .call("POST", "/api/v1/skills", Some(tenant), Some(body), None)
            .await;
        (status, body)
    }

    pub async fn publish_agent(
        &self,
        tenant: &str,
        slug: &str,
        version: &str,
        visibility: &str,
    ) -> (StatusCode, Value) {
        self.publish_agent_with(
            tenant,
            json!({
                "slug": slug,
                "version": version,
                "visibility": visibility,
                "publisher_name": "张三",
                "files": agent_files("QA Agent", "面向配置问答的智能体")
            }),
        )
        .await
    }

    pub async fn publish_agent_with(&self, tenant: &str, body: Value) -> (StatusCode, Value) {
        let (status, body, _) = self
            .call("POST", "/api/v1/agents", Some(tenant), Some(body), None)
            .await;
        (status, body)
    }

    pub async fn login(&self) -> String {
        let (status, body, cookie) = self
            .call(
                "POST",
                "/admin/api/login",
                None,
                Some(json!({ "username": ADMIN_USER, "password": ADMIN_PASSWORD })),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "admin login should succeed: {body}");
        cookie.expect("session cookie")
    }
}

// ---------------------------------------------------------------------------
// assertions
// ---------------------------------------------------------------------------

/// The listing entry for `slug`, panicking with the whole payload on a miss.
pub fn find<'a>(listing: &'a Value, slug: &str) -> &'a Value {
    listing["items"]
        .as_array()
        .unwrap_or_else(|| panic!("listing without items: {listing}"))
        .iter()
        .find(|item| item["slug"] == slug)
        .unwrap_or_else(|| panic!("{slug} missing from listing: {listing}"))
}

pub fn slugs(listing: &Value) -> Vec<String> {
    listing["items"]
        .as_array()
        .unwrap_or_else(|| panic!("listing without items: {listing}"))
        .iter()
        .map(|item| item["slug"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every key of a listing item, sorted — the field set is part of the API
/// contract, so a refactor that silently drops or renames one must fail.
pub fn field_names(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}
