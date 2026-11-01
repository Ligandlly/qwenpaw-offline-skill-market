//! Offline skill market backend for QwenPaw 2.2.2 hub tenants.
//!
//! Two planes:
//! * `/api/v1/...` — called by the QwenPaw plugin with `X-Tenant-Id`.
//! * `/admin/api/...` — separate admin login (session cookie).
//!
//! Nothing here implements installation: the QwenPaw host keeps bundle
//! validation, scanning and installation (see PLAN.md §1).

pub mod admin;
pub mod agents;
pub mod api;
pub mod auth;
pub mod config;
pub mod counters;
pub mod db;
pub mod dto;
pub mod error;
pub mod limits;
pub mod log;
pub mod market;
pub mod slug;
pub mod version;
pub mod state;
pub mod stats;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use std::sync::{Arc, Mutex};

use crate::auth::Sessions;
use crate::state::{App, Shared};

pub fn build_state(cfg: config::Config, conn: rusqlite::Connection) -> Shared {
    Arc::new(App {
        db: Mutex::new(conn),
        sessions: Mutex::new(Sessions::default()),
        logins: Mutex::new(auth::LoginLimiter::default()),
        pending: Mutex::new(crate::counters::Pending::default()),
        cfg,
    })
}

pub fn router(app: Shared) -> Router {
    let bundle_limit = DefaultBodyLimit::max(bundle_body_limit_bytes(&app.cfg));
    Router::new()
        .route("/health", get(health))
        // ---- tenant plane ----
        .route("/api/v1/limits", get(api::get_limits))
        .route("/api/v1/stats", get(stats::overview))
        .route(
            "/api/v1/skills",
            get(api::list_skills)
                .post(api::publish)
                .layer(bundle_limit),
        )
        .route("/api/v1/skills/mine", get(api::list_mine))
        .route("/api/v1/skills/lookup", get(api::lookup_skills))
        // Slug derivation for the publish page (Chinese names -> pinyin).
        .route("/api/v1/slug", get(api::preview_slug))
        // Dry run of a publish: the form shows every problem before submitting.
        .route(
            "/api/v1/skills/validate",
            post(api::validate).layer(bundle_limit),
        )
        .route("/api/v1/skills/{slug}/status", get(api::skill_status))
        .route("/api/v1/skills/{slug}/withdraw", post(api::withdraw))
        .route("/api/v1/skills/{slug}/restore", post(api::restore))
        .route(
            "/api/v1/skills/{slug}/{version}/bundle",
            get(api::fetch_bundle),
        )
        // Short alias: this is the `bundle_url` handed to MarketSpec.fetch_bundle.
        .route("/skills/{slug}/{version}", get(api::fetch_bundle))
        // ---- agent market (tenant plane) ----
        .route(
            "/api/v1/agents",
            get(agents::list_agents)
                .post(agents::publish)
                .layer(bundle_limit),
        )
        .route("/api/v1/agents/mine", get(agents::list_mine))
        .route("/api/v1/agents/lookup", get(agents::lookup_agents))
        // Dry run of a publish, mirroring /api/v1/skills/validate.
        .route(
            "/api/v1/agents/validate",
            post(agents::validate).layer(bundle_limit),
        )
        .route("/api/v1/agents/{slug}/status", get(agents::agent_status))
        .route("/api/v1/agents/{slug}/withdraw", post(agents::withdraw))
        .route("/api/v1/agents/{slug}/restore", post(agents::restore))
        .route(
            "/api/v1/agents/{slug}/{version}/bundle",
            get(agents::fetch_bundle),
        )
        // ---- admin plane ----
        // Credentials are two short strings: nothing legitimate comes close.
        .route(
            "/admin/api/login",
            post(admin::login).layer(DefaultBodyLimit::max(SMALL_BODY_LIMIT)),
        )
        .route(
            "/admin/api/logout",
            post(admin::logout).layer(DefaultBodyLimit::max(SMALL_BODY_LIMIT)),
        )
        .route("/admin/api/skills", get(admin::list_skills))
        .route("/admin/api/skills/{slug}/takedown", post(admin::takedown))
        .route("/admin/api/skills/{slug}/restore", post(admin::restore))
        .route("/admin/api/agents", get(agents::admin_list))
        .route(
            "/admin/api/agents/{slug}/takedown",
            post(agents::admin_takedown),
        )
        .route(
            "/admin/api/agents/{slug}/restore",
            post(agents::admin_restore),
        )
        .route("/admin/api/audit", get(admin::audit_log))
        // Everything that is not a bundle upload stays on axum's own 2 MiB
        // default: only the publish/validate routes get a larger transport cap
        // (see `bundle_body_limit_bytes`).
        .layer(axum::middleware::from_fn(log::request_log))
        .layer(axum::middleware::from_fn_with_state(
            app.clone(),
            budget_request,
        ))
        .with_state(app)
}

/// Ceiling for a request that carries a bundle, derived from the market quota
/// instead of guessed: one file costs at most 6x its size on the wire (JSON
/// escapes every control byte as `\u00XX`) and 4/3 as base64. The quota itself
/// is still enforced by `limits::check_bundle`, after parsing.
fn bundle_body_limit_bytes(cfg: &config::Config) -> usize {
    cfg.limits
        .max_bundle_bytes
        .saturating_mul(6)
        .saturating_add(64 * 1024)
}

/// Requests that only carry credentials or a short reason.
const SMALL_BODY_LIMIT: usize = 8 * 1024;

/// Give every request a deadline, so a stalled handler cannot pin a connection
/// and a worker thread for the rest of the process lifetime.
pub async fn budget_request(
    State(app): State<Shared>,
    request: Request,
    next: Next,
) -> Response {
    match tokio::time::timeout(app.cfg.request_timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error::ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "timeout",
            "请求处理超时，请稍后重试",
        )
        .into_response(),
    }
}

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "status": "ok" }))
}
