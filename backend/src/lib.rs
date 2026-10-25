//! Offline skill market backend for QwenPaw 2.2.2 hub tenants.
//!
//! Two planes:
//! * `/api/v1/...` — called by the QwenPaw plugin with `X-Tenant-Id`.
//! * `/admin/api/...` — separate admin login (session cookie).
//!
//! Nothing here implements installation: the QwenPaw host keeps bundle
//! validation, scanning and installation (see PLAN.md §1).

pub mod admin;
pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod limits;
pub mod log;
pub mod state;

use axum::routing::{get, post};
use axum::Router;
use std::sync::{Arc, Mutex};

use crate::auth::Sessions;
use crate::state::{App, Shared};

pub fn build_state(cfg: config::Config, conn: rusqlite::Connection) -> Shared {
    Arc::new(App {
        db: Mutex::new(conn),
        sessions: Mutex::new(Sessions::default()),
        cfg,
    })
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/health", get(health))
        // ---- tenant plane ----
        .route("/api/v1/limits", get(api::get_limits))
        .route("/api/v1/skills", get(api::list_skills).post(api::publish))
        .route("/api/v1/skills/mine", get(api::list_mine))
        .route("/api/v1/skills/lookup", get(api::lookup_skills))
        .route("/api/v1/skills/{slug}/status", get(api::skill_status))
        .route("/api/v1/skills/{slug}/withdraw", post(api::withdraw))
        .route("/api/v1/skills/{slug}/restore", post(api::restore))
        .route(
            "/api/v1/skills/{slug}/{version}/bundle",
            get(api::fetch_bundle),
        )
        // Short alias: this is the `bundle_url` handed to MarketSpec.fetch_bundle.
        .route("/skills/{slug}/{version}", get(api::fetch_bundle))
        // ---- admin plane ----
        .route("/admin/api/login", post(admin::login))
        .route("/admin/api/logout", post(admin::logout))
        .route("/admin/api/skills", get(admin::list_skills))
        .route("/admin/api/skills/{slug}/takedown", post(admin::takedown))
        .route("/admin/api/skills/{slug}/restore", post(admin::restore))
        .route("/admin/api/audit", get(admin::audit_log))
        .layer(axum::middleware::from_fn(log::request_log))
        .with_state(app)
}

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "status": "ok" }))
}
