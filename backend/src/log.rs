//! Logging: one global subscriber, level from `MARKET_LOG`.
//!
//! ponytail: no `env-filter` feature (it drags in regex) — the level is a
//! single env var, parsed once at startup into a `LevelFilter`.

use std::str::FromStr;
use std::time::Instant;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use tracing::level_filters::LevelFilter;
use tracing::{error, info, warn};

/// Parse a level name (`off`/`error`/`warn`/`info`/`debug`/`trace`).
/// Anything unknown falls back to `info`.
pub fn level(spec: &str) -> LevelFilter {
    LevelFilter::from_str(spec.trim()).unwrap_or(LevelFilter::INFO)
}

/// Install the global subscriber. Second calls are ignored (tests, reloads).
pub fn init(spec: &str) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(level(spec))
        .with_target(false)
        .try_init();
}

/// Log every request: method, path, tenant, status, latency.
///
/// 4xx is the caller's problem (warn), 5xx is ours (error).
pub async fn request_log(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let tenant = req
        .headers()
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status();
    let ms = started.elapsed().as_secs_f64() * 1000.0;

    if status.is_server_error() {
        error!(
            tenant = %tenant,
            method = %method,
            path = %path,
            status = status.as_u16(),
            ms = %format!("{ms:.1}"),
            "request failed"
        );
    } else if status.is_client_error() {
        warn!(
            tenant = %tenant,
            method = %method,
            path = %path,
            status = status.as_u16(),
            ms = %format!("{ms:.1}"),
            "request rejected"
        );
    } else {
        info!(
            tenant = %tenant,
            method = %method,
            path = %path,
            status = status.as_u16(),
            ms = %format!("{ms:.1}"),
            "request"
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels() {
        assert_eq!(level("debug"), LevelFilter::DEBUG);
        assert_eq!(level(" WARN "), LevelFilter::WARN);
        assert_eq!(level("off"), LevelFilter::OFF);
        assert_eq!(level("nonsense"), LevelFilter::INFO);
    }
}
